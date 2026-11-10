#!/usr/bin/env python3
"""Parse retained TLS matrix logs; no timing or allocation claims inferred."""
import ast
import json
from pathlib import Path
import re
import sys

root = Path(sys.argv[1])
files = {
    'baseline': 'matrix-baseline-phase.log',
    'sender': 'matrix-sender.log',
    'receiver': 'matrix-receiver.log',
    'both': 'matrix-checkpoint.log',
}
rows = []
for policy, filename in files.items():
    seen = set()
    for line in (root / filename).read_text().splitlines():
        if not line.startswith('case '):
            continue
        flags = {name: value == 'true' for name, value in re.findall(r'(new_client|new_server|server_rpk|client_rpk|offer)=(true|false)', line)}
        identity = tuple(flags.items())
        if identity in seen:
            continue
        seen.add(identity)
        compressions = ast.literal_eval(re.search(r'compressions=(\([^)]*\))', line).group(1))
        flights = ast.literal_eval(re.search(r'flights=(\[[^]]*\])', line).group(1))
        counts = {name: int(value) for name, value in re.findall(r'(allocations|reallocations|moved|frees|bytes|live|peak): (-?[0-9]+)', line)}
        rows.append(dict(policy=policy, **flags, compressions=compressions, flights=flights,
                         encrypted_tls_bytes=sum(up + down for up, down in flights), counts=counts))
    assert sum(row['policy'] == policy for row in rows) == 32, policy
# X509/X509 encrypted byte sequences and compression invocation counts are identical
# for every role/offer combination across all source variants.
keys = ('new_client', 'new_server', 'server_rpk', 'client_rpk', 'offer')
baseline = {tuple(row[key] for key in keys): row for row in rows if row['policy'] == 'baseline'}
for row in rows:
    if not row['server_rpk'] and not row['client_rpk']:
        reference = baseline[tuple(row[key] for key in keys)]
        assert row['compressions'] == reference['compressions']
        assert row['flights'] == reference['flights']
print(json.dumps({'rows': rows, 'x509_wire_equivalence': True,
                  'limits': 'Single sans-I/O samples; Rust allocations include config/drop/wire fixture; C/kernel/UDP not measured.'}, indent=2))
