//! A member of a hyper-raft group, as a process (`hyper_raft_e2e::node`).
//!
//! ```text
//! hyper-raft-node --id N --voters 1,2,3 --listen 127.0.0.1:0 --wal PATH --tick-ms T
//!                 --max-keys K --max-pending P --max-entries E
//! ```
//!
//! It prints `listening <port>` once its socket is bound, then serves until its standard input
//! ends — its parent closed it or died, so a member never outlives the test that started it — or
//! it is killed. Where the others listen it is told by the test (`Control::Peers`).
use std::{
    io::Write,
    net::UdpSocket,
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use hyper_raft_e2e::{
    node::{Node, Settings},
    wal::Wal,
};

/// What the command line says.
struct Arguments {
    settings: Settings,
    listen: String,
    wal: PathBuf,
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
            tick: Duration::from_millis(value(arguments, "--tick-ms")?),
            max_keys: value(arguments, "--max-keys")?,
            max_pending: value(arguments, "--max-pending")?,
            max_entries: value(arguments, "--max-entries")?,
        },
        listen: value(arguments, "--listen")?,
        wal: PathBuf::from(value::<String>(arguments, "--wal")?),
    })
}

fn serve(arguments: Arguments) -> Result<(), Box<dyn std::error::Error>> {
    let wal = Wal::open(
        &arguments.wal,
        arguments.settings.voters.clone(),
        arguments.settings.max_entries,
    )?;
    let socket = UdpSocket::bind(&arguments.listen)?;
    let port = socket.local_addr()?.port();
    let mut node = Node::open(arguments.settings, socket, wal)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "listening {port}")?;
    stdout.flush()?;
    drop(stdout);
    watch_parent()?;
    node.run(&PARENT_GONE)?;
    Ok(())
}

/// Set once standard input ends: the member's loop then returns, and the process with it.
static PARENT_GONE: AtomicBool = AtomicBool::new(false);

/// Watches standard input until it ends. The parent holds the pipe's other end for as long as it
/// lives, so a member never outlives the test that started it, however the test ends (a test
/// killed outright runs no clean-up of its own). One thread for the process, blocked on the pipe.
fn watch_parent() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("parent".to_owned())
        .spawn(|| {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            PARENT_GONE.store(true, Ordering::Release);
        })
        .map(drop)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().collect();
    let outcome = parse(&arguments)
        .map_err(Box::<dyn std::error::Error>::from)
        .and_then(serve);
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "hyper-raft-node: {error}");
            ExitCode::FAILURE
        }
    }
}
