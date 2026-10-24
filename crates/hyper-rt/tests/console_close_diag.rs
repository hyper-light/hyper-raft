//! hunt/console-close (a diag, never merged): a console close delivered to a child process two ways,
//! with hyper-rt's handler holding its return and without, and how long the OS waits; printed.
//!
//! - W: the child is made with a console of its own (`CREATE_NEW_CONSOLE`) and posts `WM_CLOSE` to its
//!   console's window, as the window's Close button does (Go's TestCtrlHandler, golang/go#41884).
//! - P: the child runs on a pseudoconsole the test made, and the test closes it ("Closing a
//!   pseudoconsole will send CTRL_CLOSE_EVENT to each client application that is still connected",
//!   ClosePseudoConsole).

#![cfg(windows)]
#![allow(unsafe_code)]
#![allow(clippy::all, clippy::pedantic, clippy::nursery, clippy::restriction)]

use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hyper_rt::runtime::{LocalRuntime, RuntimeConfig};
use hyper_rt::signal::{Signal, SignalStream, subscribe};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, GenerateConsoleCtrlEvent, GetConsoleWindow, HPCON,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

const ROLE: &str = "HYPER_RT_DIAG_ROLE";
const FILE: &str = "HYPER_RT_DIAG_FILE";
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CTRL_BREAK: u32 = 1;
const STOPPED: i32 = 3;
const NEVER: i32 = 4;
const SLEPT: i32 = 5;
const BROKE: i32 = 6;
const ROUNDS: usize = 10;

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

// ------------------------------------------------------------------------------------------- child

#[test]
fn child_entry() {
    let Some(role) = role() else { return };
    match role.as_str() {
        "w-stop" | "w-never" => window_child(&role),
        "p-stop" | "p-never" => pty_child(&role),
        "w-break" => break_child(),
        _ => std::process::exit(99),
    }
}

fn window_child(role: &str) -> ! {
    let signals = subscribe(&[Signal::Close]).unwrap();
    let mut rt = LocalRuntime::new(&config()).unwrap();
    // SAFETY: no preconditions.
    let window = unsafe { GetConsoleWindow() };
    println!("DIAGCHILD window={window:?}");
    if window.is_null() {
        std::process::exit(98);
    }
    // SAFETY: a window handle the console gave; WM_CLOSE carries no pointers.
    let posted = unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    println!("DIAGCHILD posted={posted}");
    let code = rt.block_on(close_then(signals, role.to_owned())).unwrap();
    std::process::exit(code);
}

fn pty_child(role: &str) -> ! {
    let signals = subscribe(&[Signal::Close]).unwrap();
    let mut rt = LocalRuntime::new(&config()).unwrap();
    println!("DIAGREADY");
    std::io::stdout().flush().unwrap();
    let code = rt.block_on(close_then(signals, role.to_owned())).unwrap();
    std::process::exit(code);
}

async fn close_then(signals: SignalStream, role: String) -> i32 {
    match hyper_rt::futures::within(30_000_000_000, signals.recv()).await {
        Ok(Some(Ok(seen))) if seen.contains(Signal::Close) => {
            if role.ends_with("never") {
                println!("DIAGCHILD got close; sleeping");
                let _ = std::io::stdout().flush();
                std::thread::sleep(Duration::from_secs(60));
                SLEPT
            } else {
                stop()
            }
        }
        other => {
            println!("DIAGCHILD no close: {other:?}");
            NEVER
        }
    }
}

fn stop() -> i32 {
    let path = std::env::var(FILE).unwrap();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"stopped in order\n").unwrap();
    file.sync_all().unwrap();
    println!("DIAGCHILD stopped");
    let _ = std::io::stdout().flush();
    STOPPED
}

fn break_child() -> ! {
    let signals = subscribe(&[Signal::Break]).unwrap();
    for _ in 0..2 {
        // SAFETY: no pointers; group 0 is every process on this console (this one alone).
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK, 0) };
        println!("DIAGCHILD break sent={sent}");
    }
    std::thread::sleep(Duration::from_secs(6));
    for (event, entered, left) in hyper_rt::signal::diag_entries() {
        println!("DIAGCHILD entry event={event} entered_ms={entered} left_ms={left}");
    }
    drop(signals);
    std::process::exit(BROKE);
}

// ------------------------------------------------------------------------------------------- W

fn spawn_window(role: &str, file: &Path, extra: &[(&str, &str)]) -> (i32, u128, String, String) {
    let started = Instant::now();
    let mut command = Command::new(me());
    command
        .args(["--exact", "child_entry", "--nocapture", "--test-threads", "1"])
        .env(ROLE, role)
        .env(FILE, file)
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    let out = command.output().unwrap();
    let ms = started.elapsed().as_millis();
    (
        out.status.code().unwrap_or(i32::MIN),
        ms,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn w_rounds() {
    if role().is_some() {
        return;
    }
    for mode in ["hold", "nohold"] {
        for round in 0..ROUNDS {
            let file = scratch(&format!("w-{mode}-{round}"));
            let extra: &[(&str, &str)] = if mode == "nohold" {
                &[("HYPER_RT_DIAG_NO_HOLD", "1")]
            } else {
                &[]
            };
            let (code, ms, out, err) = spawn_window("w-stop", &file, extra);
            let wrote = std::fs::read_to_string(&file).unwrap_or_default();
            println!(
                "DIAG w {mode} round={round} code={code} hex={:#x} elapsed_ms={ms} file={:?}",
                code as u32,
                wrote.trim()
            );
            if round == 0 {
                println!("DIAG w {mode} stdout:\n{out}\nDIAG stderr:\n{err}");
            }
            let _ = std::fs::remove_file(&file);
        }
    }
}

#[test]
fn w_never() {
    if role().is_some() {
        return;
    }
    for round in 0..2 {
        let file = scratch(&format!("w-never-{round}"));
        let (code, ms, out, err) = spawn_window("w-never", &file, &[]);
        println!("DIAG w never round={round} code={code} hex={:#x} elapsed_ms={ms}", code as u32);
        if round == 0 {
            println!("DIAG w never stdout:\n{out}\nDIAG stderr:\n{err}");
        }
    }
    let file = scratch("w-never-60s");
    let (code, ms, _, _) = spawn_window("w-never", &file, &[("HYPER_RT_DIAG_HOLD_MS", "60000")]);
    println!("DIAG w never hold60s code={code} hex={:#x} elapsed_ms={ms}", code as u32);
}

#[test]
fn w_break() {
    if role().is_some() {
        return;
    }
    let file = scratch("w-break");
    for hold in [true, false] {
        let extra: &[(&str, &str)] = if hold { &[("HYPER_RT_DIAG_HOLD_BREAK", "1")] } else { &[] };
        let (code, ms, out, err) = spawn_window("w-break", &file, extra);
        println!(
            "DIAG w break hold={hold} code={code} hex={:#x} elapsed_ms={ms}\n{out}\nDIAG stderr:\n{err}",
            code as u32
        );
    }
}

// ------------------------------------------------------------------------------------------- P

struct Pty {
    code: u32,
    ready: bool,
    close_to_exit_ms: u128,
    output: String,
}

fn spawn_pty(role: &str, file: &Path, extra: &[(&str, &str)]) -> Pty {
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
        std::env::set_var(FILE, file);
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
        let ready = ready_rx.recv_timeout(Duration::from_secs(60)).is_ok();
        let closed = Instant::now();
        ClosePseudoConsole(pty);
        let waited = WaitForSingleObject(info.hProcess, 90_000);
        let close_to_exit_ms = closed.elapsed().as_millis();
        let mut code = 0u32;
        GetExitCodeProcess(info.hProcess, &mut code);
        println!("DIAG p wait={waited:#x}");
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        CloseHandle(in_write);
        DeleteProcThreadAttributeList(list);
        let output = done_rx
            .recv_timeout(Duration::from_secs(10))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|_| "(the output pipe did not close)".to_owned());
        Pty { code, ready, close_to_exit_ms, output }
    }
}

#[test]
fn p_rounds() {
    if role().is_some() {
        return;
    }
    for mode in ["hold", "nohold"] {
        for round in 0..ROUNDS {
            let file = scratch(&format!("p-{mode}-{round}"));
            let extra: &[(&str, &str)] = if mode == "nohold" {
                &[("HYPER_RT_DIAG_NO_HOLD", "1")]
            } else {
                &[]
            };
            let pty = spawn_pty("p-stop", &file, extra);
            let wrote = std::fs::read_to_string(&file).unwrap_or_default();
            println!(
                "DIAG p {mode} round={round} ready={} code={} hex={:#x} close_to_exit_ms={} file={:?}",
                pty.ready,
                pty.code as i32,
                pty.code,
                pty.close_to_exit_ms,
                wrote.trim()
            );
            if round == 0 {
                println!("DIAG p {mode} output:\n{:?}", pty.output);
            }
            let _ = std::fs::remove_file(&file);
        }
    }
}

#[test]
fn p_never() {
    if role().is_some() {
        return;
    }
    for round in 0..2 {
        let file = scratch(&format!("p-never-{round}"));
        let pty = spawn_pty("p-never", &file, &[]);
        println!(
            "DIAG p never round={round} ready={} code={} hex={:#x} close_to_exit_ms={}",
            pty.ready, pty.code as i32, pty.code, pty.close_to_exit_ms
        );
    }
    let file = scratch("p-never-60s");
    let pty = spawn_pty("p-never", &file, &[("HYPER_RT_DIAG_HOLD_MS", "60000")]);
    println!(
        "DIAG p never hold60s ready={} code={} hex={:#x} close_to_exit_ms={}",
        pty.ready, pty.code as i32, pty.code, pty.close_to_exit_ms
    );
}
