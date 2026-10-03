//! A member of a hyper-raft group, as a process (`hyper_raft_e2e::node`).
//!
//! ```text
//! hyper-raft-node --id N --voters 1,2,3 --listen 127.0.0.1:0 --wal PATH
//!                 --max-keys K --max-pending P --max-writes W
//! ```
//!
//! It prints `listening <port>` once its socket is bound, then serves until its standard input
//! ends — its parent closed it or died, so a member never outlives the test that started it — or
//! it is killed. Where the others listen it is told by the test (`Control::Peers`).
use std::{io::Write, net::UdpSocket, path::PathBuf, process::ExitCode};

use hyper_raft_e2e::{
    node::{Node, NodeError, Settings},
    parent, run,
    wal::Wal,
    wire::{self, Kind},
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
            max_keys: value(arguments, "--max-keys")?,
            max_pending: value(arguments, "--max-pending")?,
            max_writes: value(arguments, "--max-writes")?,
        },
        listen: value(arguments, "--listen")?,
        wal: PathBuf::from(value::<String>(arguments, "--wal")?),
    })
}

fn serve(arguments: Arguments) -> Result<(), Box<dyn std::error::Error>> {
    let wal = Wal::open(
        &arguments.wal,
        arguments.settings.voters.clone(),
        arguments.settings.max_writes,
    )?;
    // Raised and durable before the member's liveness stream sends anything under it.
    let run = run::raise(&run::path(&arguments.wal)).map_err(NodeError::Run)?;
    let socket = UdpSocket::bind(&arguments.listen)?;
    let me = socket.local_addr()?;
    let port = me.port();
    let mut node = Node::open(arguments.settings, run, socket, wal)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "listening {port}")?;
    stdout.flush()?;
    drop(stdout);
    // The parent's going wakes the member with an empty control datagram, which it takes and
    // drops (`hyper_raft_e2e::parent`); one too long to seal is sent with no bytes, which wakes
    // the member as well.
    let mut wake = Vec::new();
    wire::begin(&mut wake, Kind::Control);
    if !wire::seal(&mut wake, wire::MAX_DATAGRAM) {
        wake.clear();
    }
    parent::watch(me, std::thread::current(), wake)?;
    node.run(&parent::PARENT_GONE)?;
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
            let _ = writeln!(std::io::stderr(), "hyper-raft-node: {error}");
            ExitCode::FAILURE
        }
    }
}
