#!/bin/bash
# The back-to-back request case alone, Karlin's rule against Karlin-or-park, alternating, 20 rounds.
bin=/tmp/hr-spin-bin2
rules=(karlin karlin-or-park)
c="rtt n=40000 warm=4000"
for round in $(seq 0 19); do
  for step in 0 1; do
    rule=${rules[$(((round + step) % 2))]}
    load=$(sysctl -n vm.loadavg | awk '{print $2}')
    echo "process round=$round rule=$rule load=$load case=\"$c\""
    # shellcheck disable=SC2086
    HYPER_RT_SPIN_RULE=$rule "$bin" hyper $c
  done
done
