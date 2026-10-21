#!/usr/bin/env bash
# Run the Python (timing.py) and Rust (timing.rs) harnesses back to back on
# the Pi. Both append to benchmarks/timings.csv.
#
# Blocks run in ORDER (default "py rs rs py"), so slow drift (warm-up, room
# temperature) doesn't line up with one library. Each block runs TRIALS
# recorded trials (default 15, so 30 per library with the default order).
# Any other arguments are passed to both harnesses.
#
# Usage, ideally inside tmux so a dropped SSH session doesn't kill the run:
#     benchmarks/run_all.sh
#     TRIALS=3 ORDER="py rs" benchmarks/run_all.sh --counts 1 10   # quick check
#
# Environment:
#     TRIALS       recorded trials per block (default 15)
#     ORDER        blocks to run, "py" and/or "rs" (default "py rs rs py")
#     NO_PULL=1    skip `git pull --ff-only`
#     ALLOW_DIRTY=1  run even with uncommitted changes to the crate or harnesses
#
# Before recording, the script checks that Cargo.lock is committed and the
# crate and harness sources are clean (so library_source has no -dirty), that
# the trigger file is writable (bulk methods need it), and that `sudo -n`
# works (both harnesses use it to disable bus search).
set -euo pipefail

cd "$(dirname "$0")/.."
TRIALS="${TRIALS:-15}"
ORDER="${ORDER:-py rs rs py}"
PY=benchmarks/venv/bin/python
RS=target/release/examples/timing

mkdir -p benchmarks/logs
log="benchmarks/logs/run_$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$log") 2>&1
echo "log: $log"

die() { echo "run_all: $*" >&2; exit 1; }

for block in $ORDER; do
    [[ $block == py || $block == rs ]] || die "ORDER may only contain py and rs, got '$block'"
done

if [[ -z ${NO_PULL:-} ]]; then
    git pull --ff-only
fi

git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1 \
    || die "Cargo.lock is not committed. Build once, commit it, then rerun."
dirty=$(git status --porcelain -- src Cargo.toml Cargo.lock rust-toolchain.toml \
    benchmarks/timing.py benchmarks/timing.rs)
if [[ -n $dirty && -z ${ALLOW_DIRTY:-} ]]; then
    die "uncommitted changes (results would be marked -dirty):
$dirty
Commit them, or set ALLOW_DIRTY=1."
fi

[[ -x $PY ]] || die "$PY not found. Create the venv in benchmarks/venv first."
sudo -n true 2>/dev/null || die "passwordless sudo is needed to disable bus search."

# Only bus masters with DS18B20s matter; the DS2413 bus is never triggered.
found_bus=
for bus in /sys/bus/w1/devices/w1_bus_master*; do
    grep -q '^28-' "$bus/w1_master_slaves" 2>/dev/null || continue
    found_bus=1
    [[ -w $bus/therm_bulk_read ]] \
        || die "$bus/therm_bulk_read is not writable by $(whoami); bulk methods would fail."
done
[[ -n $found_bus ]] || die "no bus master with DS18B20s under /sys/bus/w1/devices."

cargo build --release --locked --example timing

echo "order: $ORDER, $TRIALS trials per block, extra args: ${*:-none}"
echo "commit: $(git rev-parse --short=12 HEAD)"
for block in $ORDER; do
    echo "=== $block: $(date -u +%H:%M:%SZ) ==="
    if [[ $block == py ]]; then
        "$PY" benchmarks/timing.py --trials "$TRIALS" "$@"
    else
        "$RS" --trials "$TRIALS" "$@"
    fi
done
echo "=== done: $(date -u +%H:%M:%SZ) ==="
