#!/usr/bin/env python3
"""Baseline benchmark: w1thermsensor sequential reads.

Reads the first N sensors (sorted by ROM ID) one after another, the way
w1thermsensor's documented multi-sensor pattern does, and appends one row
per trial to timings.csv. Must run on the Raspberry Pi.

Usage:
    python timing.py                       # 30 trials at counts 1, 2, 5, 10
    python timing.py --trials 3            # quick smoke test
    python timing.py --counts 1 5 10 --trials 50
    python timing.py --seed 1234 --gap 2   # reproducible order, 2 s idle between trials
"""
import argparse
import csv
import json
import os
import platform
import random
import resource
import socket
import sys
import time
from datetime import datetime, timezone
from importlib.metadata import PackageNotFoundError, distribution
from pathlib import Path

from w1thermsensor import Unit, W1ThermSensor

LIBRARY = "w1thermsensor"
CSV_PATH = Path(__file__).with_name("timings.csv")
DEFAULT_COUNTS = [1, 2, 5, 10]

# Shared schema: the w1-therm-api and rs1wire harnesses should write the
# same columns so all results can live in one file. Columns that don't apply
# to a harness are left empty (e.g. the phase timings for sequential reads).
FIELDS = [
    "run_id", "timestamp_utc", "library", "library_version", "library_source",
    "method", "read_path", "sensor_count", "bus_sensor_count", "trial",
    "seed", "gap_s",
    "wall_s", "trigger_s", "wait_s", "readout_s",
    "cpu_total_s", "cpu_user_s", "cpu_sys_s",
    "reads_ok", "reads_failed", "error_types",
    "resolution_bits", "conv_time_ms",
    "kernel", "os", "python", "hostname",
]


def parse_args():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--trials", type=int, default=30,
                   help="recorded trials per sensor count (default 30)")
    p.add_argument("--warmup", type=int, default=1,
                   help="unrecorded warmup trials per sensor count (default 1)")
    p.add_argument("--counts", type=int, nargs="+", default=DEFAULT_COUNTS,
                   help="sensor counts to test (default 1 2 5 10)")
    p.add_argument("--seed", type=int, default=None,
                   help="seed for the count-order shuffle (default: random, recorded)")
    p.add_argument("--gap", type=float, default=0.0,
                   help="idle seconds after each trial, untimed (default 0: back to back)")
    return p.parse_args()


def library_source(dist):
    """'pypi' for a normal install, or the VCS URL and commit for a git install
    (e.g. the PR #120 branch), taken from pip's direct_url.json (PEP 610)."""
    raw = dist.read_text("direct_url.json")
    if not raw:
        return "pypi"
    info = json.loads(raw)
    vcs = info.get("vcs_info")
    if vcs:
        return f"{info.get('url', '?')}@{vcs.get('commit_id', '?')}"
    return info.get("url", "local")


def environment():
    """Metadata recorded with every row, per the harness rules."""
    try:
        os_name = platform.freedesktop_os_release().get("PRETTY_NAME", "unknown")
    except OSError:
        os_name = "unknown"
    try:
        dist = distribution(LIBRARY)
    except PackageNotFoundError:
        sys.exit(f"{LIBRARY} is not installed in this environment.")
    return {
        "library": LIBRARY,
        "library_version": dist.version,
        "library_source": library_source(dist),
        "method": "sequential",
        "read_path": "w1_slave",
        "kernel": platform.release(),
        "os": os_name,
        "python": platform.python_version(),
        "hostname": socket.gethostname(),
    }


def device_attr(sensors, name):
    """Read a per-device sysfs attribute (e.g. 'resolution', 'conv_time') from
    each sensor; returns one value, or 'a;b' if they differ."""
    values = set()
    for s in sensors:
        try:
            values.add(s.sensorpath.parent.joinpath(name).read_text().strip())
        except OSError:
            values.add("unknown")
    return ";".join(sorted(values))


def bus_sensor_count(sensors):
    """Total slaves on the bus master(s) these sensors hang off, not just the
    ones being read. A bulk trigger converts every device on the master."""
    values = set()
    for s in sensors:
        master = s.sensorpath.parent.resolve().parent
        try:
            values.add(master.joinpath("w1_master_slave_count").read_text().strip())
        except OSError:
            values.add("unknown")
    return ";".join(sorted(values))


def open_csv(path):
    """Open for appending; refuse to mix schemas with an existing file."""
    is_new = not path.exists() or path.stat().st_size == 0
    if not is_new:
        with path.open(newline="") as f:
            header = next(csv.reader(f), [])
        if header != FIELDS:
            sys.exit(f"{path.name} exists with a different header. "
                     "Rename or empty it, then rerun.")
    f = path.open("a", newline="")
    writer = csv.DictWriter(f, fieldnames=FIELDS)
    if is_new:
        writer.writeheader()
    return f, writer


def run_trial(sensors):
    """Read every sensor once, sequentially. Returns timing and error counts."""
    ok = 0
    errors = []
    ru0 = resource.getrusage(resource.RUSAGE_SELF)
    cpu0 = time.process_time()
    t0 = time.perf_counter()
    for s in sensors:
        try:
            s.get_temperature(Unit.DEGREES_C)
            ok += 1
        except Exception as e:  # record every failure type rather than crash
            errors.append(type(e).__name__)
    wall = time.perf_counter() - t0
    cpu = time.process_time() - cpu0
    ru1 = resource.getrusage(resource.RUSAGE_SELF)
    return {
        "wall_s": f"{wall:.6f}",
        # process_time() is the precise total; getrusage's user/sys split is
        # apportioned from tick sampling, so treat the split as approximate.
        "cpu_total_s": f"{cpu:.6f}",
        "cpu_user_s": f"{ru1.ru_utime - ru0.ru_utime:.6f}",
        "cpu_sys_s": f"{ru1.ru_stime - ru0.ru_stime:.6f}",
        "reads_ok": ok,
        "reads_failed": len(errors),
        "error_types": ";".join(sorted(set(errors))),
    }


def main():
    args = parse_args()
    counts = sorted(set(args.counts))
    seed = args.seed if args.seed is not None else int.from_bytes(os.urandom(4), "big")
    rng = random.Random(seed)

    # Discovery happens once, outside the timed region.
    sensors = sorted(W1ThermSensor.get_available_sensors(), key=lambda s: s.id)
    if len(sensors) < max(counts):
        sys.exit(f"Found {len(sensors)} sensors but need {max(counts)}. "
                 "Check wiring and /sys/bus/w1/devices/.")

    env = environment()
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    per_count = {
        n: {
            "resolution_bits": device_attr(sensors[:n], "resolution"),
            "conv_time_ms": device_attr(sensors[:n], "conv_time"),
            "bus_sensor_count": bus_sensor_count(sensors[:n]),
        }
        for n in counts
    }

    print(f"run {run_id}: {env['library']} {env['library_version']} "
          f"({env['library_source']}), kernel {env['kernel']}, {env['os']}")
    print(f"seed {seed}, gap {args.gap}s")
    print(f"sensors (sorted): {', '.join(s.id for s in sensors[:max(counts)])}")

    f, writer = open_csv(CSV_PATH)
    try:
        for trial in range(-args.warmup, args.trials):
            # Shuffle count order each trial so slow drift (temperature,
            # background load) doesn't line up with one sensor count.
            order = counts[:]
            rng.shuffle(order)
            for n in order:
                result = run_trial(sensors[:n])
                if args.gap:
                    time.sleep(args.gap)
                if trial < 0:
                    continue
                writer.writerow({
                    "run_id": run_id,
                    "timestamp_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                    "sensor_count": n,
                    "trial": trial,
                    "seed": seed,
                    "gap_s": args.gap,
                    **per_count[n],
                    **env,
                    **result,
                })
                f.flush()  # keep completed trials if the run is interrupted
            label = "warmup" if trial < 0 else f"trial {trial + 1}/{args.trials}"
            print(f"  {label} done")
    finally:
        f.close()

    print(f"Results appended to {CSV_PATH}")


if __name__ == "__main__":
    main()
