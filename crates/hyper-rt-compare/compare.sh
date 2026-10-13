#!/usr/bin/env bash
# The comparison as docs/benchmarks.md records it: mantle's cases (its research note 42 §7–§8), each
# (case, arm) in a fresh process so each runtime starts as its consumer starts it, every round visiting
# the arms in an order rotated by one from the round before, the one-minute load average read before
# each process. Prints every process's lines, then for each case and arm the median over the rounds of
# its headline numbers and their range.
#
#   bash compare.sh [rounds]     (default 5)
set -euo pipefail
cd "$(dirname "$0")"
rounds="${1:-5}"
cargo build --release --quiet
binary="${CARGO_TARGET_DIR:-target}/release/hyper-rt-compare"
arms=(hyper hyper-mpsc slates tokio-ct tokio-mt)
cases=(
  "yield n=1000000 tasks=16"
  "spawn n=200000 batch=1000"
  "pingpong n=20000 warm=2000"
  "pingpong n=100000 warm=10000 local=1"
  "pingpong n=40000 warm=4000 local=1 work_ns=1000"
  "rtt n=40000 warm=4000 clients=8"
  "rtt n=8000 warm=800 clients=8 busy_us=50"
  "rtt n=8000 warm=800 clients=8 busy_us=50 yield=wake"
  "rtt n=5000 warm=500 gap_us=200"
  "rtt n=20000 warm=2000"
  "timer n=2000 d_us=1000"
)
raw=$(mktemp "${TMPDIR:-/tmp}/rt-compare.XXXXXX")
trap 'rm -f "$raw"' EXIT
load() {
  if [ -r /proc/loadavg ]; then cut -d' ' -f1 /proc/loadavg; else sysctl -n vm.loadavg | awk '{print $2}'; fi
}
count=${#arms[@]}
for round in $(seq 0 $((rounds - 1))); do
  for c in "${cases[@]}"; do
    for step in $(seq 0 $((count - 1))); do
      arm=${arms[$(((round + step) % count))]}
      before=$(load)
      # shellcheck disable=SC2086
      out=$("$binary" "$arm" $c)
      {
        echo "process round=$round arm=$arm load=$before case=\"$c\""
        echo "$out"
      } | tee -a "$raw"
    done
  done
done
python3 - "$raw" <<'EOF'
import re, statistics, sys
rows = {}
loads = []
case = arm = None
for line in open(sys.argv[1]):
    m = re.match(r'process round=(\d+) arm=(\S+) load=(\S+) case="(.*)"', line)
    if m:
        arm, case = m.group(2), m.group(4)
        loads.append(float(m.group(3)))
        rows.setdefault((case, arm), []).append({})
        continue
    kv = dict(re.findall(r'(\w+)=([\d.]+)', line))
    if line.startswith('cost '):
        rows[(case, arm)][-1].update({'ops_s': float(kv['ops_per_s']), 'cpu': float(kv['cpu_ns_per_op'])})
        if kv.get('instructions_per_op'):
            rows[(case, arm)][-1]['ins'] = float(kv['instructions_per_op'])
    for name in ('rtt', 'oneway', 'late'):
        if line.startswith(f'lat.{name} ') and 'p50_ns' in kv:
            rows[(case, arm)][-1][name] = (float(kv['p50_ns']), float(kv['p99_ns']), float(kv['p999_ns']))
    if line.startswith('busy ') and 'share' in kv:
        rows[(case, arm)][-1]['share'] = float(kv['share'])
print(f"load average {min(loads):.1f} to {max(loads):.1f} (median {statistics.median(loads):.1f})")
print("| Case | Arm | Operations a second | CPU ns an operation | Instructions an operation | p50 / p99 / p99.9 µs | Busy task's share |")
print("|---|---|---|---|---|---|---|")
def span(values, fmt):
    return f"{fmt(statistics.median(values))} ({fmt(min(values))}–{fmt(max(values))})"
for (case, arm), runs in rows.items():
    ops = [r['ops_s'] for r in runs if 'ops_s' in r]
    cpu = [r['cpu'] for r in runs if 'cpu' in r]
    ins = [r['ins'] for r in runs if 'ins' in r]
    lat = None
    for name in ('oneway', 'rtt', 'late'):
        got = [r[name] for r in runs if name in r]
        if got and (name != 'rtt' or 'oneway' not in runs[0]):
            lat = (name, got)
            break
    lat_cell = '—'
    if lat:
        name, got = lat
        lat_cell = name + ' ' + ' / '.join(f"{statistics.median(g[i] for g in got) / 1e3:.1f}" for i in range(3))
    share = [r['share'] for r in runs if 'share' in r]
    share_cell = span(share, lambda v: f'{v:.2f}') if share else '—'
    print(f"| {case} | {arm} | {span(ops, lambda v: f'{v:,.0f}')} | {span(cpu, lambda v: f'{v:,.1f}')} | {span(ins, lambda v: f'{v:,.0f}') if ins else '—'} | {lat_cell} | {share_cell} |")
EOF
