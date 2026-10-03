#!/usr/bin/env python3
"""Writes hyper-liveness's simulated worlds (crates/hyper-liveness/tests/support/worlds.rs) from the
trace tool's output, per host: `hyper-timing-trace timer`, `quantiles` of a plain run and of a
flushed one, and `freezes` of the plain run; macOS's, then Linux's.

    python3 scripts/liveness-worlds.py \
        mac-timer.txt mac-plain.txt mac-flush.txt mac-freezes.txt \
        linux-timer.txt linux-plain.txt linux-flush.txt linux-freezes.txt \
        > crates/hyper-liveness/tests/support/worlds.rs
    cargo fmt --all

docs/benchmarks.md, "The simulation's worlds", has the runs and the commands that made the inputs.
"""
import sys
import textwrap

def table(values):
    assert len(values) == 107, len(values)
    vals = [int(v) for v in values]
    # A quantile function never falls: the trace tool's tables are sorted samples.
    assert all(a <= b for a, b in zip(vals, vals[1:])), vals
    return vals

def parse_timer(text):
    timer, flush = [], None
    for line in text.splitlines():
        p = line.split()
        if not p: continue
        if p[0] == 'late' and p[1] in ('select', 'ppoll'):
            timer.append((int(p[2]), table(p[3:])))
        if p[0] == 'flush' and len(p) == 109:
            flush = table(p[2:])
    return timer, flush

def parse_quantiles(text):
    """The one run `quantiles` printed: its header and its tables."""
    run = {}
    for line in text.splitlines():
        if line.startswith('# ') and 'interval' in line:
            assert 'meta' not in run, 'one run a file'
            run['meta'] = line[2:]
            continue
        p = line.split()
        if p and p[0] in ('delay', 'flush'):
            run[p[0]] = table(p[1:])
    return run

def parse_freezes(text):
    """The one run `freezes` printed: its header, its span and its freezes."""
    run = {'at': []}
    for line in text.splitlines():
        p = line.split()
        if line.startswith('# '):
            run['meta'] = line[2:]
        elif p and p[0] == 'span':
            run['span'] = int(p[1])
        elif p and p[0] == 'freeze':
            run['at'].append((int(p[1]), int(p[2])))
    # Sorted and apart, as the tool merges them.
    assert all(a + l <= b for (a, l), (b, _) in zip(run['at'], run['at'][1:])), run['at']
    return run

(mac_timer_text, mac_plain, mac_flush, mac_freezes,
 linux_timer_text, linux_plain, linux_flush, linux_freezes) = (open(path).read() for path in sys.argv[1:9])
mp, mf, mz = parse_quantiles(mac_plain), parse_quantiles(mac_flush), parse_freezes(mac_freezes)
lp, lf, lz = parse_quantiles(linux_plain), parse_quantiles(linux_flush), parse_freezes(linux_freezes)
mac_timer, mac_sweep_flush = parse_timer(mac_timer_text)
linux_timer, linux_sweep_flush = parse_timer(linux_timer_text)

def head(text):
    for line in text.splitlines():
        if line.startswith('# timer sweep'):
            return line[2:]
    return ''

def doc_lines(doc):
    """The doc comment, its sentences joined and wrapped to the repository's 100 columns."""
    return [f'/// {line}' for line in textwrap.wrap(' '.join(doc), width=96)]

def arr(name, vals, doc):
    lines = doc_lines(doc)
    body = ', '.join(f'{v}' for v in vals)
    return '\n'.join(lines) + f'\npub(crate) static {name}: Table = [{body}];\n'

def freezes_arr(name, run, doc):
    lines = doc_lines(doc)
    body = ', '.join(f'({a}, {l})' for a, l in run['at'])
    return '\n'.join(lines) + f"\npub(crate) static {name}: Freezes = Freezes {{\n    span: {run['span']},\n    at: &[{body}],\n}};\n"

def timer_arr(name, rows, doc):
    lines = doc_lines(doc)
    out = '\n'.join(lines) + f'\npub(crate) static {name}: [(u64, Table); {len(rows)}] = [\n'
    for asked, vals in rows:
        out += f'    ({asked}, [{", ".join(str(v) for v in vals)}]),\n'
    return out + '];\n'

src = f'''//! The simulation's worlds, as the heartbeat traces measured them (`docs/benchmarks.md`, "The
//! simulation's worlds", has the runs, their loads and the commands). Each table is a quantity's
//! quantiles at [`GRID`], whole nanoseconds, as `hyper-timing-trace quantiles` and `timer` print
//! them; the simulation draws from each by the inverse transform. Generated from the trace tool's
//! output, never edited by hand.

#![allow(clippy::unreadable_literal)]

/// A quantity's quantiles at [`GRID`], whole nanoseconds.
pub(crate) type Table = [u64; 107];

/// A host's freezes over a run (`hyper-timing-trace freezes`): the run's span, and each freeze's
/// onset from the run's start and its length, nanoseconds, in order.
#[derive(Debug)]
pub(crate) struct Freezes {{
    pub(crate) span: u64,
    pub(crate) at: &'static [(u64, u64)],
}}

/// The probabilities of every table: every hundredth, and past the 99th the tail a run's stalls
/// live in, to the most (`hyper-timing-trace`'s `analyse::GRID`).
pub(crate) const GRID: [f64; 107] = {{
    let mut grid = [0.0; 107];
    let mut i = 0;
    while i < 100 {{
        grid[i] = i as f64 / 100.0;
        i += 1;
    }}
    let tail = [0.995, 0.999, 0.9995, 0.9999, 0.99995, 0.99999, 1.0];
    let mut j = 0;
    while j < tail.len() {{
        grid[100 + j] = tail[j];
        j += 1;
    }}
    grid
}};

'''
src += arr('MACOS_DELAY', mp['delay'], ['macOS: the one-way delay from the send to the receiver\'s kernel stamp, heartbeats every 1 ms on', f'loopback ({mp["meta"].split(": ",1)[1]}).'])
src += freezes_arr('MACOS_FREEZES', mz, ['macOS: the host\'s freezes over the same run, in which it ran neither of its processes', f'({mz["meta"].split(": ",1)[1]}).'])
src += arr('MACOS_FLUSH', mf['flush'], ['macOS: a block written and flushed with `F_FULLFSYNC` before each heartbeat, every 10 ms', f'({mf["meta"].split(": ",1)[1]}).'])
src += arr('MACOS_BUSY_FLUSH', mac_sweep_flush, ['macOS: a block written and flushed with `F_FULLFSYNC` back to back, 2,000 times: the flush of a', f'log its groups keep busy ({head(mac_timer_text)}).'])
src += timer_arr('MACOS_TIMER', mac_timer, ['macOS: how late a wait on a socket (`select(2)`) ends past what it asked, 2,000 waits at each', f'asked duration of the 1-2-5 grid from 1 µs to 10 ms ({head(mac_timer_text)}).'])
src += arr('LINUX_DELAY', lp['delay'], ['Linux in Docker Desktop\'s VM: the one-way delay from the send to the receiver\'s kernel stamp,', f'heartbeats every 2 ms on loopback ({lp["meta"].split(": ",1)[1]}).'])
src += freezes_arr('LINUX_FREEZES', lz, ['Linux in its VM: the host\'s freezes over the same run, in which it ran neither of its processes', f'({lz["meta"].split(": ",1)[1]}).'])
src += arr('LINUX_FLUSH', lf['flush'], ['Linux in its VM: a block written and flushed with `fdatasync` before each heartbeat, every 2 ms', f'({lf["meta"].split(": ",1)[1]}).'])
src += timer_arr('LINUX_TIMER', linux_timer, ['Linux in its VM: how late a wait on a socket (`ppoll(2)`) ends past what it asked, 2,000 waits', f'at each asked duration ({head(linux_timer_text)}).'])
src += '''/// Windows: the default period of the clock interrupt a timed wait ends on, 15.625 ms (Microsoft,
/// `timeBeginPeriod`; `docs/research/timing.md`, "Winsock timestamping" and the timer notes). The
/// trace tool does not record on Windows.
pub(crate) const WINDOWS_TICK: u64 = 15_625_000;

/// Windows: a full flush (`FlushFileBuffers`). hyper-durable-e2e's members measured their floors
/// `E[flush] + G` at 20 to 32 ms with `G` at 2 to 8 ms on the windows-2025 and windows-11-arm
/// runners (`docs/timing.md` §2.9, "On real detectors"): mean flushes of 12 to 30 ms. No shape
/// within that span was measured, so a flush here is uniform over it: the law of greatest entropy
/// a bounded span alone gives (Jaynes, "Information Theory and Statistical Mechanics", Phys. Rev.
/// 106, 1957).
pub(crate) static WINDOWS_FLUSH: Table = {
    let (low, high) = (12_000_000u64, 30_000_000u64);
    let mut table = [0u64; 107];
    let mut i = 0;
    while i < 107 {
        table[i] = low + ((high - low) as f64 * GRID[i]) as u64;
        i += 1;
    }
    table
};
'''
sys.stdout.write(src)
