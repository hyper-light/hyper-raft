//! A member of a hyper-raft group, as a process (`hyper_raft_e2e::node`).
//!
//! ```text
//! hyper-raft-node --id N --voters 1,2,3 --listen 127.0.0.1:0 --wal PATH --tick-ms T
//!                 --deadline-ms D --max-keys K --max-pending P --max-entries E
//! ```
//!
//! It prints `listening <port>` once its socket is bound, then serves until `D` milliseconds
//! have passed or it is killed. Where the others listen it is told by the test (`Control::Peers`).
use std::{
    io::Write,
    net::UdpSocket,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
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
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(value(arguments, "--deadline-ms")?))
        .ok_or("a deadline past what the clock counts")?;
    Ok(Arguments {
        settings: Settings {
            id: value(arguments, "--id")?,
            voters,
            tick: Duration::from_millis(value(arguments, "--tick-ms")?),
            deadline,
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
    node.run()?;
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
