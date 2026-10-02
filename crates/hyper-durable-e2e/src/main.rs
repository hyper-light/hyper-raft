//! A member of a hyper-durable group, as a process (`hyper_durable_e2e::node`).
//!
//! ```text
//! hyper-durable-node --id N --voters 1,2,3 --listen 127.0.0.1:0 --log PATH --period-ms T
//!                    --max-keys K --max-pending P
//! ```
//!
//! It prints `listening <port>` once its socket is bound, then serves until its standard input
//! ends (the test is gone) or it is killed. Stopped at an armed point it prints `stopped <point>`
//! and waits to be killed; fenced by a failed write it prints `fenced` and exits with status 3,
//! for the test to start it again on its log.
use std::io::Write;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::Duration;

use hyper_durable_e2e::control::{self, Order};
use hyper_durable_e2e::node::{Node, NodeError, Settings, open_log};
use hyper_raft_e2e::wire;

/// The status a member exits with once a failed write fenced it.
const FENCED: u8 = 3;

struct Arguments {
    settings: Settings,
    listen: String,
    log: PathBuf,
}

fn value<T: std::str::FromStr>(arguments: &[String], name: &str) -> Result<T, String> {
    let at = arguments
        .iter()
        .position(|argument| argument == name)
        .ok_or_else(|| format!("{name} is missing"))?;
    arguments
        .get(at.saturating_add(1))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("{name} has no value that reads"))
}

fn parse(arguments: &[String]) -> Result<Arguments, String> {
    let voters: String = value(arguments, "--voters")?;
    let voters = voters
        .split(',')
        .map(|voter| voter.parse::<u64>().map_err(|_| format!("a voter {voter}")))
        .collect::<Result<Vec<u64>, String>>()?;
    Ok(Arguments {
        settings: Settings {
            id: value(arguments, "--id")?,
            voters,
            period: Duration::from_millis(value(arguments, "--period-ms")?),
            max_keys: value(arguments, "--max-keys")?,
            max_pending: value(arguments, "--max-pending")?,
        },
        listen: value(arguments, "--listen")?,
        log: PathBuf::from(value::<String>(arguments, "--log")?),
    })
}

/// Watches standard input until it ends: the parent holds its end for as long as it lives, so a
/// member never outlives its test. One thread, blocked on the pipe.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn watch_parent() -> std::io::Result<Receiver<()>> {
    let (gone, parent) = sync_channel(1);
    std::thread::Builder::new()
        .name("parent".to_owned())
        .spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            let _ = gone.try_send(());
        })
        .map(|_| parent)
}

/// Turns the member's waker into a datagram to its own socket, so its one wait on the socket
/// covers the log's answers too. One thread, blocked on the waker's channel.
#[allow(
    clippy::disallowed_methods,
    reason = "real processes on the host's clock, threads and environment (CLAUDE.md §1a, end to end)"
)]
fn relay(socket: &UdpSocket) -> std::io::Result<std::task::Waker> {
    let (tell, told) = sync_channel::<usize>(1024);
    let (waker, _) = hyper_measure::wake::waker(0, tell);
    let me = socket.local_addr()?;
    let out = UdpSocket::bind("127.0.0.1:0")?;
    let mut datagram = Vec::new();
    control::put_order(&mut datagram, 0, &Order::Wake);
    let sealed = wire::seal(&mut datagram, wire::MAX_DATAGRAM);
    std::thread::Builder::new()
        .name("wake".to_owned())
        .spawn(move || {
            while told.recv().is_ok() {
                if sealed {
                    let _ = out.send_to(&datagram, me);
                }
            }
        })?;
    Ok(waker)
}

fn serve(arguments: Arguments) -> Result<(), Box<dyn std::error::Error>> {
    let log = open_log(&arguments.log)?;
    let socket = UdpSocket::bind(&arguments.listen)?;
    let port = socket.local_addr()?.port();
    let waker = relay(&socket)?;
    let mut node = Node::open(arguments.settings, socket, log, waker)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "listening {port}")?;
    stdout.flush()?;
    drop(stdout);
    let parent = watch_parent()?;
    if let Some(point) = node.run(&parent)? {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "stopped {}", point.name())?;
        stdout.flush()?;
        drop(stdout);
        // Stopped where the test armed it: it waits to be killed, or for the test to go.
        let _ = parent.recv();
    }
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let outcome = parse(&arguments)
        .map_err(Box::<dyn std::error::Error>::from)
        .and_then(serve);
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let fenced = error
                .downcast_ref::<NodeError>()
                .is_some_and(|e| matches!(e, NodeError::Fenced(_)));
            let mut stdout = std::io::stdout().lock();
            if fenced {
                let _ = writeln!(stdout, "fenced");
                let _ = stdout.flush();
            }
            let _ = writeln!(std::io::stderr(), "hyper-durable-node: {error}");
            if fenced {
                ExitCode::from(FENCED)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}
