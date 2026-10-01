#!/usr/bin/env bash
# The comparison as docs/benchmarks.md records it: each (implementation, size) row in a fresh
# process, every run visiting the rows in an order rotated by one from the run before, so that
# neither implementation always runs first or after the other; the one-minute load average is
# read before each row. Prints every row, then the median of each cell's per-run medians with
# their range, and per round the process's allocations, reallocations and bytes and the datagrams
# the asking side sent and received.
#
#   bash compare.sh [runs] [rounds]     (defaults: 9 runs; 2,000 rounds, 250 at 512 KiB)
set -euo pipefail
cd "$(dirname "$0")"
runs="${1:-9}"
rounds="${2:-}"
cargo build --release --quiet
binary=target/release/hyper-transport-compare
cells=()
for size in 64 4096 65536 524288; do
  for who in focal hyper; do
    cells+=("$who $size")
  done
done
count=${#cells[@]}
raw=$(mktemp "${TMPDIR:-/tmp}/compare.XXXXXX")
trap 'rm -f "$raw"' EXIT
load() {
  if [ -r /proc/loadavg ]; then cut -d' ' -f1 /proc/loadavg; else sysctl -n vm.loadavg | awk '{print $2}'; fi
}
for run in $(seq 0 $((runs - 1))); do
  for step in $(seq 0 $((count - 1))); do
    cell=${cells[$(((run + step) % count))]}
    before=$(load)
    # shellcheck disable=SC2086
    row=$("$binary" $cell $rounds)
    echo "$run $before $row" | tee -a "$raw"
  done
done
python3 - "$raw" <<'EOF'
import statistics, sys
rows = {}
loads = []
for line in open(sys.argv[1]):
    run, load, who, size, median, allocs, reallocs, nbytes, datagrams = line.split()
    loads.append(float(load))
    rows.setdefault((int(size), who), []).append((float(median), float(allocs), float(reallocs), float(nbytes), float(datagrams)))
print(f"load average {min(loads):.1f} to {max(loads):.1f} (median {statistics.median(loads):.1f})")
print("| Size each way | Implementation | Median round (range) | Allocations | Reallocations | Bytes | Datagrams |")
print("|---|---|---|---|---|---|---|")
for (size, who), cell in sorted(rows.items()):
    times = [c[0] for c in cell]
    med = lambda i: statistics.median(c[i] for c in cell)
    print(f"| {size} B | {who} | {statistics.median(times):,.0f} µs ({min(times):,.0f}–{max(times):,.0f}) | {med(1):,.1f} | {med(2):,.2f} | {med(3):,.0f} | {med(4):,.1f} |")
EOF
