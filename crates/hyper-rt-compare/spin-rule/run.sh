#!/bin/bash
# Interleaved A/B of the idle spin rules (runtime.md §15 item 8 prototype): each round visits every
# (case, rule) once in an order rotated by the round, a process each, the load average before each.
rounds="${1:-5}"
bin=/tmp/hr-spin-bin
rules=(karlin learned random)
cases=(
  "rtt n=5000 warm=1000 gap_us=200"
  "rtt n=20000 warm=2000"
  "rtt n=40000 warm=4000 clients=8"
  "pingpong n=20000 warm=2000"
  "rtt n=5000 warm=1000 gap_us=20"
)
for round in $(seq 0 $((rounds - 1))); do
  for c in "${cases[@]}"; do
    for step in 0 1 2; do
      rule=${rules[$(((round + step) % 3))]}
      load=$(sysctl -n vm.loadavg | awk '{print $2}')
      echo "process round=$round rule=$rule load=$load case=\"$c\""
      # shellcheck disable=SC2086
      HYPER_RT_SPIN_RULE=$rule "$bin" hyper $c
    done
  done
done
