#!/usr/bin/env bash
# Check the model of docs/models with TLC (docs/models/README.md). CI runs it on Linux
# (.github/workflows/ci.yml, job `model`); CLAUDE.md §1 keeps it off the gates.
#
#   scripts/check-model.sh             every configuration below
#   scripts/check-model.sh NAME...     the ones named
#
# A configuration passes or is refused. One that passes holds every invariant it names and has
# exactly the distinct states it states. One that is refused is the model with a rule of the core
# removed, or with a claim that must be false: the checker must find the invariant written beside
# it violated, and find it at exactly the states the configuration states. A model that cannot
# reach the defect a rule is for checks nothing of the rule.
#
# The checker is TLC from tla2tools.jar v1.7.4, taken by its SHA-256 into TLA_TOOLS (default
# target/tla) and kept there. Java is the one on PATH; this script installs nothing else.
#
# Nothing the checker takes grows without a bound.
#   States   A configuration states its distinct states (StateBudget) and the checker stops at
#            one more (WithinBudget).
#   Memory   TLC_MEMORY_MB of heap, and as much again of direct memory, where TLC keeps its
#            fingerprints: the checker can take no more, whatever the machine has. The default
#            is what the largest configuration was measured to need (docs/models/README.md).
#   Disk     The states of one run, under TLA_TOOLS, removed when the run ends, however it ends.
#   Threads  TLC_WORKERS, one by default. A refused configuration always runs with one, so that
#            the checker stops at the same state every time.
#   Time     The CI job's timeout (.github/workflows/ci.yml).
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
tools="${TLA_TOOLS:-$root/target/tla}"
jar="$tools/tla2tools.jar"
release=v1.7.4
digest=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
memory="${TLC_MEMORY_MB:-256}"
workers="${TLC_WORKERS:-1}"

# NAME CONFIGURATION REFUSED [MODULE]: the invariant the checker must find violated, or - for one
# that passes; the module checked, FastTrack unless named (a scenario, FastTrackScenario, is
# FastTrack held to an order of leaders and proposals).
configurations="
one          FastTrack.cfg                  -
round        FastTrackRound.cfg             -
four         FastTrackFour.cfg              -
change       FastTrackChange.cfg            -
grow         FastTrackGrow.cfg              -
classic      Classic.cfg                    -
joint        ClassicJoint.cfg               -
reached      FastTrackReached.cfg           NoFastByHeld
anyround     FastTrackAnyRound.cfg          LeaderHolds
least        FastTrackWrong.cfg             LeaderHolds
anyconfig    FastTrackAnyConfig.cfg         LeaderHolds
growreached  FastTrackGrowReached.cfg       NoFastByHeldAfterChange
marked       Marked.cfg                     -
markedchange MarkedChange.cfg               -
markedjoint  MarkedJoint.cfg                -
markedself   MarkedSelf.cfg                 LeaderHolds
markedwhole  MarkedWhole.cfg                LeaderHolds
markedreach  MarkedReached.cfg              NoMarkedLeader
changereach  MarkedChangeReached.cfg        NoMarkedLeader
jointreach   MarkedJointReached.cfg         NoMarkedLeader
scenario     FastTrackScenario.cfg          -              FastTrackScenario
before       FastTrackScenarioBefore.cfg    LeaderHolds    FastTrackScenario
covered      FastTrackScenarioCovered.cfg   LeaderHolds    FastTrackScenario
logs         FastTrackScenarioLogs.cfg      LeaderHolds    FastTrackScenario
ballot       FastTrackScenarioBallot.cfg    LeaderHolds    FastTrackScenario
designb      FastTrackScenarioB.cfg         -              FastTrackScenario
designblog   FastTrackScenarioBLog.cfg      -              FastTrackScenario
roundballot  FastTrackRoundBallot.cfg       -
oneb         FastTrackB.cfg                 -
"

if ! command -v java >/dev/null 2>&1; then
  echo "TLC needs Java on PATH" >&2
  exit 1
fi
mkdir -p "$tools"
sum() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}
if [ ! -f "$jar" ] || [ "$(sum "$jar")" != "$digest" ]; then
  curl -sSfL -o "$jar.part" \
    "https://github.com/tlaplus/tlaplus/releases/download/$release/tla2tools.jar"
  if [ "$(sum "$jar.part")" != "$digest" ]; then
    rm -f "$jar.part"
    echo "tla2tools.jar is not the one this script names" >&2
    exit 1
  fi
  mv "$jar.part" "$jar"
fi

java -version 2>&1 | head -1

work=""
trap 'if [ -n "$work" ]; then rm -rf "${work:?}"; fi' EXIT
# A signal ends the script by way of the trap above.
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

check() { # NAME CONFIGURATION REFUSED [MODULE]
  local name="$1" config="$2" refused="$3" module="${4:-FastTrack}"
  local budget threads status=0 found
  budget="$(sed -n 's/^ *StateBudget *= *\([0-9][0-9]*\) *$/\1/p' "$root/docs/models/$config")"
  if [ -z "$budget" ]; then
    echo "$config states no StateBudget" >&2
    return 1
  fi
  threads="$workers"
  if [ "$refused" != "-" ]; then
    threads=1
  fi
  work="$(mktemp -d "$tools/run-$name.XXXXXX")"
  cp "$root"/docs/models/*.tla "$root/docs/models/$config" "$work/"
  echo "== $name ($config, $module)"
  (cd "$work" && java "-Xmx${memory}m" "-XX:MaxDirectMemorySize=${memory}m" -XX:+UseParallelGC \
    -cp "$jar" tlc2.TLC -workers "$threads" -deadlock -metadir "$work/states" \
    -config "$config" "$module.tla" >out.log 2>&1) || status=$?
  grep -E "states generated|Invariant .* is violated|^Error|Finished in" "$work/out.log" | tail -5 \
    || true
  found="$(sed -n 's/^[0-9]* states generated, \([0-9]*\) distinct states found.*/\1/p' \
    "$work/out.log" | tail -1)"
  # TLC exits 12 when an invariant is violated.
  if [ "$status" -eq 12 ] && grep -q "Invariant WithinBudget is violated" "$work/out.log"; then
    echo "$config has more than the $budget states it states: count them, and state them" >&2
    return 1
  fi
  if [ "$refused" != "-" ]; then
    if [ "$status" -ne 12 ] || ! grep -q "Invariant $refused is violated" "$work/out.log"; then
      echo "the checker did not find $refused violated in $config (exit $status)" >&2
      return 1
    fi
  elif [ "$status" -ne 0 ]; then
    tail -40 "$work/out.log" >&2
    return "$status"
  fi
  if [ "$found" != "$budget" ]; then
    echo "$config has $found states and states $budget: state what it has" >&2
    return 1
  fi
  if [ "$refused" != "-" ]; then
    echo "$name: the checker finds $refused violated at $found states"
  else
    echo "$name: $found states, every invariant holds"
  fi
  rm -rf "${work:?}"
  work=""
}

if [ "$#" -eq 0 ]; then
  set -- $(echo "$configurations" | awk 'NF { print $1 }')
fi
# Every configuration named runs, whatever an earlier one did, so that one run reports every
# count; the script fails at the end if any did not end as it states.
failed=""
for name in "$@"; do
  row="$(echo "$configurations" | awk -v name="$name" '$1 == name')"
  if [ -z "$row" ]; then
    echo "unknown configuration: $name" >&2
    exit 2
  fi
  # shellcheck disable=SC2086
  if ! check $row; then
    failed="$failed $name"
    if [ -n "$work" ]; then
      rm -rf "${work:?}"
      work=""
    fi
  fi
done
if [ -n "$failed" ]; then
  echo "did not end as stated:$failed" >&2
  exit 1
fi
