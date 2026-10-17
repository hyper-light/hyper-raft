import re, statistics, sys, collections
rows = collections.defaultdict(list)
loads = []
cur = None
for line in open(sys.argv[1]):
    m = re.match(r'process round=(\d+) rule=(\S+) load=(\S+) case="(.*)"', line)
    if m:
        cur = (m.group(4), m.group(2)); rows[cur].append({}); loads.append(float(m.group(3))); continue
    kv = dict(re.findall(r'(\w+)=([\d.]+)', line))
    if line.startswith('cost '):
        rows[cur][-1].update(ops=float(kv['ops_per_s']), cpu=float(kv['cpu_ns_per_op']))
    if line.startswith('lat.rtt '):
        rows[cur][-1].update(p50=float(kv['p50_ns']), p99=float(kv['p99_ns']), p999=float(kv['p999_ns']))
print(f"load {min(loads):.1f}-{max(loads):.1f} median {statistics.median(loads):.1f}; runs per cell {min(len(v) for v in rows.values())}")
print("| Case | Rule | n | Requests a second | CPU ns a request | rtt p50 / p99 / p99.9 µs |")
print("|---|---|---|---|---|---|")
def span(vals, f):
    return f"{f(statistics.median(vals))} ({f(min(vals))}–{f(max(vals))})"
for (case, rule), runs in rows.items():
    runs = [r for r in runs if 'cpu' in r]
    if not runs: continue
    lat = " / ".join(f"{statistics.median(r[k] for r in runs)/1e3:.1f}" for k in ('p50','p99','p999'))
    print(f"| {case} | {rule} | {len(runs)} | {span([r['ops'] for r in runs], lambda v: f'{v:,.0f}')} | {span([r['cpu'] for r in runs], lambda v: f'{v:,.0f}')} | {lat} |")
