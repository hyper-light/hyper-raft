#!/usr/bin/env bash
# The hunt for hyper-log's power-loss hang on linux/arm64 (fate-commit e15ee61, CI 39948749085:
# `every_acknowledged_update_survives_power_loss` ran until the gate's cancel, every other test of
# crates/hyper-log/tests/log.rs passed in 60 s; not reproduced in ~250k generated cases on macOS
# arm64 and Linux x86_64). The log tests' binary is run again and again until the job's budget,
# each run with its own proptest seed and bounded at 300 s (the binary takes ~60 s whole): a run
# past its bound is stopped, every thread's stack taken (gdb), then killed. `single` runs the one
# test alone; `whole` runs the binary whole, its tests in parallel as the gate runs them.
# Results: target/hunt/ (hangs.txt, hang-<seed>.txt stacks, hang-<seed>.out output, progress.txt).
set -u
lane=$1
minutes=$2
out=target/hunt
mkdir -p "$out"
cargo test -p hyper-log --test log --no-run --locked 2>&1 | tail -3
bin=$(ls -t target/debug/deps/log-* | grep -v '\.d$' | head -1)
echo "binary $bin lane $lane" | tee "$out/progress.txt"
case "$lane" in
  single) args=(--exact every_acknowledged_update_survives_power_loss --test-threads=1) ;;
  *) args=() ;;
esac
end=$(( $(date +%s) + minutes * 60 ))
i=0
while [ "$(date +%s)" -lt "$end" ]; do
  i=$((i + 1))
  seed=$(od -An -N8 -tu8 /dev/urandom | tr -d ' ')
  began=$(date +%s)
  PROPTEST_RNG_SEED=$seed timeout --signal=STOP 300 "$bin" "${args[@]}" > "$out/run.txt" 2>&1 &
  pid=$!
  hung=0
  while kill -0 "$pid" 2>/dev/null; do
    child=$(pgrep -P "$pid" | head -1)
    if [ -n "$child" ] && [ "$(ps -o stat= -p "$child" | cut -c1)" = "T" ]; then
      hung=1
      echo "HANG run $i seed $seed" | tee -a "$out/hangs.txt"
      grep -E "has been running for over|^test .* \.\.\. " "$out/run.txt" | tail -5 | tee -a "$out/hangs.txt"
      sudo gdb -p "$child" -batch -ex "info threads" -ex "thread apply all bt" > "$out/hang-$seed.txt" 2>&1
      cp "$out/run.txt" "$out/hang-$seed.out"
      kill -KILL "$child"
      break
    fi
    sleep 2
  done
  wait "$pid" 2>/dev/null
  took=$(( $(date +%s) - began ))
  if [ "$hung" = 1 ]; then
    echo "run $i HANG seed $seed ${took}s" >> "$out/progress.txt"
  elif grep -q "test result: ok" "$out/run.txt"; then
    echo "run $i ok ${took}s" >> "$out/progress.txt"
  else
    echo "run $i FAIL seed $seed ${took}s" | tee -a "$out/hangs.txt" >> "$out/progress.txt"
    cp "$out/run.txt" "$out/fail-$seed.out"
  fi
done
echo "runs $i, hangs $(grep -c '^HANG' "$out/hangs.txt" 2>/dev/null || echo 0)" | tee -a "$out/progress.txt"
