//! A process that appends to a log until it is killed (`hyper_log_e2e`).
//!
//! ```text
//! hyper-log-writer PATH GROUPS ROUNDS
//! ```
//!
//! It opens the log at `PATH`, or creates it when the file is new, and goes on from where each
//! group's log ends: every round submits each group's next update, then waits for every answer
//! and prints each acknowledged one as `ack <group> <index> <start>`, flushing its stdout after
//! each. After `ROUNDS` rounds it closes the log and prints `done`.
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use hyper_log::{Log, LogError, Pending};
use hyper_log_e2e::{ID, config, open, update};

fn run(
    path: &std::path::Path,
    groups: usize,
    rounds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let fresh = !path.exists();
    let file = open(path, fresh)?;
    let log = if fresh {
        Log::create(file, config(groups), ID)?
    } else {
        Log::open(file, config(groups), ID)?.0
    };
    let mut next = Vec::with_capacity(groups);
    for g in 0..groups {
        let group = u128::try_from(g)?;
        let last = log.view(group)?.map_or(0, |v| v.last);
        next.push(last.checked_add(1).ok_or("an index past u64")?);
    }
    let mut stdout = std::io::stdout().lock();
    for _ in 0..rounds {
        let mut out: Vec<(u128, u64, Pending)> = Vec::with_capacity(groups);
        for (g, index) in next.iter().enumerate() {
            let group = u128::try_from(g)?;
            out.push((
                group,
                *index,
                log.submit_waiting(group, update(group, *index))?,
            ));
        }
        for (group, index, pending) in out {
            pending.wait()?;
            let start = log.view(group)?.map_or(0, |v| v.start.index);
            writeln!(stdout, "ack {group} {index} {start}")?;
            stdout.flush()?;
            let at = usize::try_from(group)?;
            if let Some(n) = next.get_mut(at) {
                *n = index
                    .checked_add(1)
                    .ok_or(LogError::Config("an index past u64"))?;
            }
        }
    }
    log.close()?;
    writeln!(stdout, "done")?;
    stdout.flush()?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = (
        args.first().map(PathBuf::from),
        args.get(1).and_then(|a| a.parse::<usize>().ok()),
        args.get(2).and_then(|a| a.parse::<u64>().ok()),
    );
    let (Some(path), Some(groups), Some(rounds)) = parsed else {
        let _ = writeln!(std::io::stderr(), "hyper-log-writer PATH GROUPS ROUNDS");
        return ExitCode::FAILURE;
    };
    match run(&path, groups, rounds) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "hyper-log-writer: {e}");
            ExitCode::FAILURE
        }
    }
}
