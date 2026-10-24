//! hunt/console-close (a diag, never merged): a console close delivered to a child process two ways,
//! with hyper-rt's handler holding its return and without, and how long the OS waits; printed.
//!
//! - W: the child is made with a console of its own (`CREATE_NEW_CONSOLE`) and posts `WM_CLOSE` to its
//!   console's window, as the window's Close button does (Go's TestCtrlHandler, golang/go#41884).
//! - P: the child runs on a pseudoconsole the test made, and the test closes it ("Closing a
//!   pseudoconsole will send CTRL_CLOSE_EVENT to each client application that is still connected",
//!   ClosePseudoConsole).
//!
//! Every wait here is bounded and short, and the child writes its account to a file of its own, so
//! a step that goes wrong still prints what it saw.

#![cfg(windows)]
#![allow(unsafe_code)]
#![allow(clippy::all, clippy::pedantic, clippy::nursery, clippy::restriction)]

use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt::signal::{Signal, SignalStream, subscribe};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, GenerateConsoleCtrlEvent, GetConsoleWindow, HPCON,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

const ROLE: &str = "HYPER_RT_DIAG_ROLE";
const FILE: &str = "HYPER_RT_DIAG_FILE";
const LOG: &str = "HYPER_RT_DIAG_LOG";
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CTRL_BREAK: u32 = 1;
const STOPPED: i32 = 3;
const NEVER: i32 = 4;
const SLEPT: i32 = 5;
const BROKE: i32 = 6;
const ROUNDS: usize = 4;
/// The child's patience for the close.
const CHILD_PATIENCE_NS: u64 = 8_000_000_000;
/// The parent's patience for a child's end.
const PARENT_PATIENCE: Duration = Duration::from_secs(30);

fn config() -> RuntimeConfig {
    RuntimeConfig {
        shards: 1,
        tasks_per_shard: 64,
        timers_per_shard: 64,
        interests_per_shard: 64,
        ring_entries: 64,
        step_budget_ns: 1_000_000_000,
        timer_tick_ns: 100_000,
        batch: 64,
        pin: false,
        cores: Vec::new(),
        page_bytes: 4096,
        spin_ns: 0,
        wake_tracking: None,
        latency_objective_ns: None,
    }
}

fn role() -> Option<String> {
    std::env::var(ROLE).ok()
}

fn me() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hyper-rt-console-diag-{}-{name}", std::process::id()))
}

static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// The child's account: one line appended to its log file, with the ms since it began.
fn note(line: &str) {
    let at = ORIGIN.get_or_init(Instant::now).elapsed().as_millis();
    if let Ok(path) = std::env::var(LOG) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "  child +{at}ms {line}");
            let _ = file.sync_all();
        }
    }
}

// ------------------------------------------------------------------------------------------- child

#[test]
fn child_entry() {
    let Some(role) = role() else { return };
    ORIGIN.get_or_init(Instant::now);
    note(&format!("began role={role} pid={}", std::process::id()));
    match role.as_str() {
        "w-stop" | "w-never" => window_child(&role),
        "p-stop" | "p-never" => pty_child(&role),
        "w-break" => break_child(),
        _ => std::process::exit(99),
    }
}

fn window_child(role: &str) -> ! {
    let signals = subscribe(&[Signal::Close]).unwrap();
    note("subscribed");
    let mut rt = LocalRuntime::new(&config()).unwrap();
    // SAFETY: no preconditions.
    let window = unsafe { GetConsoleWindow() };
    note(&format!("window={window:?}"));
    if window.is_null() {
        note("no window: exit 98");
        std::process::exit(98);
    }
    // SAFETY: a window handle the console gave; WM_CLOSE carries no pointers.
    let posted = unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    // SAFETY: no preconditions.
    let error = unsafe { GetLastError() };
    note(&format!("posted={posted} last_error={error}"));
    let code = rt.block_on(close_then(signals, role.to_owned())).unwrap();
    end(code)
}

fn pty_child(role: &str) -> ! {
    let signals = subscribe(&[Signal::Close]).unwrap();
    note("subscribed");
    let mut rt = LocalRuntime::new(&config()).unwrap();
    println!("DIAGREADY");
    let flushed = std::io::stdout().flush();
    note(&format!("said ready: {flushed:?}"));
    let code = rt.block_on(close_then(signals, role.to_owned())).unwrap();
    end(code)
}

fn end(code: i32) -> ! {
    for (event, entered, left) in hyper_rt::signal::diag_entries() {
        note(&format!(
            "handler call event={event} entered_ms={entered} left_ms={}",
            if left == u64::MAX { "held".to_owned() } else { left.to_string() }
        ));
    }
    note(&format!("exiting with {code}"));
    std::process::exit(code);
}

async fn close_then(signals: SignalStream, role: String) -> i32 {
    match hyper_rt::futures::within(CHILD_PATIENCE_NS, signals.recv()).await {
        Ok(Some(Ok(seen))) if seen.contains(Signal::Close) => {
            note("close received");
            if role.ends_with("never") {
                note("sleeping 60 s");
                std::thread::sleep(Duration::from_secs(60));
                SLEPT
            } else {
                stop()
            }
        }
        other => {
            note(&format!("no close: {other:?}"));
            NEVER
        }
    }
}

fn stop() -> i32 {
    let path = std::env::var(FILE).unwrap();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"stopped in order\n").unwrap();
    file.sync_all().unwrap();
    note("stop written");
    STOPPED
}

fn break_child() -> ! {
    let signals = subscribe(&[Signal::Break]).unwrap();
    for _ in 0..2 {
        // SAFETY: no pointers; group 0 is every process on this console (this one alone).
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK, 0) };
        note(&format!("break sent={sent}"));
    }
    std::thread::sleep(Duration::from_secs(6));
    drop(signals);
    end(BROKE)
}

// ------------------------------------------------------------------------------------------- W

fn spawn_window(role: &str, name: &str, extra: &[(&str, &str)]) -> String {
    let file = scratch(&format!("{name}.stop"));
    let log = scratch(&format!("{name}.log"));
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&log);
    let started = Instant::now();
    let mut command = Command::new(me());
    command
        .args(["--exact", "child_entry", "--nocapture", "--test-threads", "1"])
        .env(ROLE, role)
        .env(FILE, &file)
        .env(LOG, &log)
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in extra {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let outcome = loop {
        if let Some(status) = child.try_wait().unwrap() {
            let code = status.code().unwrap_or(i32::MIN);
            break format!("code={code} hex={:#x}", code as u32);
        }
        if started.elapsed() > PARENT_PATIENCE {
            let _ = child.kill();
            let _ = child.wait();
            break "TIMEOUT (killed)".to_owned();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let ms = started.elapsed().as_millis();
    let wrote = std::fs::read_to_string(&file).unwrap_or_default();
    let account = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&log);
    format!("{outcome} ended_after_ms={ms} file={:?}\n{account}", wrote.trim())
}

#[test]
fn w_rounds() {
    if role().is_some() {
        return;
    }
    for mode in ["hold", "nohold"] {
        for round in 0..ROUNDS {
            let extra: &[(&str, &str)] = if mode == "nohold" {
                &[("HYPER_RT_DIAG_NO_HOLD", "1")]
            } else {
                &[]
            };
            let seen = spawn_window("w-stop", &format!("w-{mode}-{round}"), extra);
            println!("DIAG w {mode} round={round} {seen}");
        }
    }
}

#[test]
fn w_never() {
    if role().is_some() {
        return;
    }
    let seen = spawn_window("w-never", "w-never", &[]);
    println!("DIAG w never hold20s {seen}");
    let seen = spawn_window("w-never", "w-never-60", &[("HYPER_RT_DIAG_HOLD_MS", "60000")]);
    println!("DIAG w never hold60s {seen}");
}

#[test]
fn w_break() {
    if role().is_some() {
        return;
    }
    for hold in [true, false] {
        let extra: &[(&str, &str)] = if hold { &[("HYPER_RT_DIAG_HOLD_BREAK", "1")] } else { &[] };
        let seen = spawn_window("w-break", &format!("w-break-{hold}"), extra);
        println!("DIAG w break hold={hold} {seen}");
    }
}

// ------------------------------------------------------------------------------------------- P

fn spawn_pty(role: &str, name: &str, extra: &[(&str, &str)]) -> String {
    let file = scratch(&format!("{name}.stop"));
    let log = scratch(&format!("{name}.log"));
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(&log);
    // SAFETY (all blocks below): the documented pseudoconsole sequence (Creating a Pseudoconsole
    // session, Microsoft Learn); every handle is one this function made and closes once.
    unsafe {
        let (mut in_read, mut in_write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert!(CreatePipe(&mut in_read, &mut in_write, std::ptr::null(), 0) != 0);
        let (mut out_read, mut out_write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert!(CreatePipe(&mut out_read, &mut out_write, std::ptr::null(), 0) != 0);
        let mut pty: HPCON = 0;
        let made = CreatePseudoConsole(COORD { X: 120, Y: 40 }, in_read, out_write, 0, &mut pty);
        assert_eq!(made, 0, "CreatePseudoConsole");
        CloseHandle(in_read);
        CloseHandle(out_write);

        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut list_bytes = vec![0u8; size];
        let list = list_bytes.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        assert!(InitializeProcThreadAttributeList(list, 1, 0, &mut size) != 0);
        assert!(
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                pty as *const core::ffi::c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) != 0
        );
        let mut start: STARTUPINFOEXW = std::mem::zeroed();
        start.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        start.lpAttributeList = list;

        std::env::set_var(ROLE, role);
        std::env::set_var(FILE, &file);
        std::env::set_var(LOG, &log);
        for (key, value) in extra {
            std::env::set_var(key, value);
        }
        let mut line: Vec<u16> = format!(
            "\"{}\" --exact child_entry --nocapture --test-threads 1",
            me().display()
        )
        .encode_utf16()
        .chain([0])
        .collect();
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            &start.StartupInfo,
            &mut info,
        );
        std::env::remove_var(ROLE);
        std::env::remove_var(FILE);
        std::env::remove_var(LOG);
        for (key, _) in extra {
            std::env::remove_var(key);
        }
        assert!(created != 0, "CreateProcessW: {}", std::io::Error::last_os_error());

        let mut reader = std::fs::File::from_raw_handle(out_read);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut buf = [0u8; 4096];
            let mut told = false;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        all.extend_from_slice(&buf[..n]);
                        if !told && all.windows(9).any(|w| w == b"DIAGREADY") {
                            told = true;
                            let _ = ready_tx.send(());
                        }
                    }
                }
            }
            let _ = done_tx.send(all);
        });
        let begun = Instant::now();
        let ready = ready_rx.recv_timeout(Duration::from_secs(20)).is_ok();
        let ready_ms = begun.elapsed().as_millis();
        let closed = Instant::now();
        ClosePseudoConsole(pty);
        let close_returned_ms = closed.elapsed().as_millis();
        let waited = WaitForSingleObject(info.hProcess, PARENT_PATIENCE.as_millis() as u32);
        let close_to_exit_ms = closed.elapsed().as_millis();
        let outcome = if waited == 0 {
            let mut code = 0u32;
            GetExitCodeProcess(info.hProcess, &mut code);
            format!("code={} hex={code:#x}", code as i32)
        } else {
            TerminateProcess(info.hProcess, 77);
            format!("TIMEOUT wait={waited:#x} (terminated)")
        };
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        CloseHandle(in_write);
        DeleteProcThreadAttributeList(list);
        let output = done_rx
            .recv_timeout(Duration::from_secs(5))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|_| "(the output pipe had not closed 5 s after)".to_owned());
        let wrote = std::fs::read_to_string(&file).unwrap_or_default();
        let account = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&log);
        format!(
            "ready={ready} ready_ms={ready_ms} close_returned_ms={close_returned_ms} {outcome} close_to_exit_ms={close_to_exit_ms} file={:?}\n{account}  output: {:?}",
            wrote.trim(),
            output.chars().take(600).collect::<String>()
        )
    }
}

#[test]
fn p_rounds() {
    if role().is_some() {
        return;
    }
    for mode in ["hold", "nohold"] {
        for round in 0..ROUNDS {
            let extra: &[(&str, &str)] = if mode == "nohold" {
                &[("HYPER_RT_DIAG_NO_HOLD", "1")]
            } else {
                &[]
            };
            let seen = spawn_pty("p-stop", &format!("p-{mode}-{round}"), extra);
            println!("DIAG p {mode} round={round} {seen}");
        }
    }
}

#[test]
fn p_never() {
    if role().is_some() {
        return;
    }
    let seen = spawn_pty("p-never", "p-never", &[]);
    println!("DIAG p never hold20s {seen}");
    let seen = spawn_pty("p-never", "p-never-60", &[("HYPER_RT_DIAG_HOLD_MS", "60000")]);
    println!("DIAG p never hold60s {seen}");
}
