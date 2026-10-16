#!/usr/bin/env python3
"""Baseline benchmark: w1thermsensor sequential, w1-therm-api sequential,
and w1-therm-api bulk reads.

Every trial runs each method at each sensor count, reading the first N
sensors (sorted by ROM ID), and appends one row per (method, N) to
timings.csv. Must run on the Raspberry Pi.

Methods:
    w1thermsensor-seq   get_temperature() on each sensor in turn (w1_slave)
    w1-therm-api-seq    read_temperatures() without a trigger, so the kernel
                        converts each sensor on read (temperature file)
    w1-therm-api-bulk   start_conversion() + wait_for_conversion() (what
                        convert() does, split so the phases can be timed),
                        then read_temperatures()

Within a trial the (method, N) cells run in a seeded random order; rows are
written sorted by method, then N, and run_order records the real order.

A bulk trigger converts every sensor on the bus master, not just the N being
read. Sensors left unread keep a pending result that the kernel hands to the
next read without converting, which would make a following sequential cell
look fast. So after each bulk cell, every sensor that wasn't read is drained
(untimed), and therm_bulk_read is checked before every cell.

If the full bus (EXPECTED_SENSORS) is discovered, the kernel's periodic
slave search is disabled on each bus master before timing starts, so search
cycles can't hold the bus mutex mid-trial. The original setting is restored
on exit. Writing w1_master_search needs root; the script uses `sudo -n tee`
for that one write, so the rest of the script runs unprivileged in the venv.

Usage:
    python timing.py                       # 30 trials at counts 1, 2, 5, 10
    python timing.py --trials 3            # quick smoke test
    python timing.py --counts 1 5 10 --trials 50
    python timing.py --methods w1-therm-api-bulk --trials 2
    python timing.py --seed 1234 --gap 2   # reproducible order, 2 s idle between cells
"""
import argparse
import csv
import json
import os
import platform
import random
import resource
import signal
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from importlib.metadata import PackageNotFoundError, distribution
from pathlib import Path

from w1_therm_api import W1Bus
from w1thermsensor import Unit, W1ThermSensor

CSV_PATH = Path(__file__).with_name("timings.csv")
DEFAULT_COUNTS = [1, 2, 5, 10]
EXPECTED_SENSORS = 10  # search is disabled only when the full bus is present

# label: (distribution name, method, default read_path). Output rows are
# sorted in this order.
METHODS = {
    "w1thermsensor-seq": ("w1thermsensor", "sequential", "w1_slave"),
    "w1-therm-api-seq": ("w1-therm-api", "sequential", "temperature"),
    "w1-therm-api-bulk": ("w1-therm-api", "bulk", "temperature"),
}

# Shared schema: the rs1wire harness should write the same columns so all
# results can live in one file. Columns that don't apply to a method are
# left empty (e.g. the phase timings for sequential reads).
FIELDS = [
    "run_id", "timestamp_utc", "library", "library_version", "library_source",
    "method", "read_path", "sensor_count", "bus_sensor_count", "trial",
    "run_order", "seed", "gap_s",
    "wall_s", "trigger_s", "wait_s", "readout_s",
    "cpu_total_s", "cpu_user_s", "cpu_sys_s",
    "reads_ok", "reads_failed", "error_types",
    "resolution_bits", "conv_time_ms", "w1_master_search",
    "kernel", "os", "python", "hostname",
]


def parse_args():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--trials", type=int, default=30,
                   help="recorded trials (default 30); each runs every method at every count")
    p.add_argument("--warmup", type=int, default=1,
                   help="unrecorded warmup trials (default 1)")
    p.add_argument("--counts", type=int, nargs="+", default=DEFAULT_COUNTS,
                   help="sensor counts to test (default 1 2 5 10)")
    p.add_argument("--methods", nargs="+", choices=list(METHODS), default=list(METHODS),
                   help="methods to run (default: all)")
    p.add_argument("--seed", type=int, default=None,
                   help="seed for the cell-order shuffle (default: random, recorded)")
    p.add_argument("--gap", type=float, default=0.0,
                   help="idle seconds after each cell, untimed (default 0: back to back)")
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


def library_meta(name):
    try:
        dist = distribution(name)
    except PackageNotFoundError:
        sys.exit(f"{name} is not installed in this environment.")
    return {
        "library": name,
        "library_version": dist.version,
        "library_source": library_source(dist),
    }


def common_env():
    """Metadata recorded with every row, per the harness rules."""
    try:
        os_name = platform.freedesktop_os_release().get("PRETTY_NAME", "unknown")
    except OSError:
        os_name = "unknown"
    return {
        "kernel": platform.release(),
        "os": os_name,
        "python": platform.python_version(),
        "hostname": socket.gethostname(),
    }


def read_attr(path):
    try:
        return path.read_text().strip()
    except OSError:
        return "unknown"


def device_attr(sensors, name):
    """Read a per-device sysfs attribute (e.g. 'resolution', 'conv_time') from
    each sensor; returns one value, or 'a;b' if they differ."""
    return ";".join(sorted({read_attr(s.sensorpath.parent / name) for s in sensors}))


def bus_master(sensor):
    """The w1_bus_masterN directory a sensor hangs off (resolves the
    /sys/bus/w1/devices symlink)."""
    return sensor.sensorpath.parent.resolve().parent


def bus_sensor_count(sensors):
    """Total slaves on the bus master(s) these sensors hang off, not just the
    ones being read. A bulk trigger converts every device on the master."""
    masters = {bus_master(s) for s in sensors}
    return ";".join(sorted(read_attr(m / "w1_master_slave_count") for m in masters))


def write_sysfs(path, value):
    """Write a root-only sysfs attribute. Tries a direct write first (works if
    already root), then `sudo -n tee` (non-interactive, fails instead of
    prompting). Returns None on success or an error message."""
    try:
        path.write_text(value)
        return None
    except PermissionError:
        pass
    except OSError as e:
        return str(e)
    r = subprocess.run(["sudo", "-n", "tee", str(path)], input=value, text=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    return None if r.returncode == 0 else (r.stderr.strip() or f"sudo exited {r.returncode}")


def search_state(masters):
    return ";".join(sorted(read_attr(m / "w1_master_search") for m in masters))


def bulk_pending(masters):
    """True if any master's therm_bulk_read reports a conversion in progress
    (-1) or results not yet read (1)."""
    return any(read_attr(m / "therm_bulk_read") in ("-1", "1") for m in masters)


def drain(paths):
    """Read each sensor's temperature file to consume any pending bulk
    result. Untimed; failures are ignored since the result is discarded."""
    for p in paths:
        try:
            (Path(p) / "temperature").read_text()
        except OSError:
            pass


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


def measure(body):
    """Run body() and time it. body returns (ok, errors, extra), where extra
    holds phase timings or other per-row columns."""
    ru0 = resource.getrusage(resource.RUSAGE_SELF)
    cpu0 = time.process_time()
    t0 = time.perf_counter()
    ok, errors, extra = body()
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
        **extra,
    }


def run_w1thermsensor(sensors):
    """Read every sensor once, sequentially."""
    def body():
        ok = 0
        errors = []
        for s in sensors:
            try:
                s.get_temperature(Unit.DEGREES_C)
                ok += 1
            except Exception as e:  # record every failure type rather than crash
                errors.append(type(e).__name__)
        return ok, errors, {}
    return measure(body)


class W1ThermApiRunner:
    """Wraps one W1Bus. Counts fallbacks from the temperature file to
    w1_slave parsing so read_path shows which path each row used."""

    def __init__(self, bus):
        self.bus = bus
        self.fallbacks = 0
        original = bus.parse_w1_slave

        def counted(sensor_path):
            self.fallbacks += 1
            return original(sensor_path)

        # read_temperature calls self.parse_w1_slave, so an instance
        # attribute takes precedence. Only adds a call on the fallback path.
        bus.parse_w1_slave = counted

    def _readout(self, paths):
        self.bus.sensors = paths
        temps = [t for _, t in self.bus.read_temperatures()]
        ok = sum(t is not None for t in temps)
        return ok, ["None"] * (len(temps) - ok)

    def _read_path(self):
        if self.fallbacks:
            return f"temperature;w1_slave_fallback={self.fallbacks}"
        return "temperature"

    def sequential(self, paths):
        self.fallbacks = 0

        def body():
            ok, errors = self._readout(paths)
            return ok, errors, {}
        result = measure(body)
        result["read_path"] = self._read_path()
        return result

    def bulk(self, paths):
        """Returns (result, readout_done). If the trigger or wait fails the
        readout is skipped, since read_temperatures() would then silently do
        per-sensor conversions instead."""
        self.fallbacks = 0
        done = []

        def body():
            t0 = time.perf_counter()
            triggered = self.bus.start_conversion()
            t1 = time.perf_counter()
            phases = {"trigger_s": f"{t1 - t0:.6f}"}
            if not triggered:
                return 0, ["TriggerFailed"] * len(paths), phases
            converted = self.bus.wait_for_conversion()
            t2 = time.perf_counter()
            phases["wait_s"] = f"{t2 - t1:.6f}"
            if not converted:
                return 0, ["WaitFailed"] * len(paths), phases
            ok, errors = self._readout(paths)
            phases["readout_s"] = f"{time.perf_counter() - t2:.6f}"
            done.append(True)
            return ok, errors, phases
        result = measure(body)
        result["read_path"] = self._read_path()
        return result, bool(done)


def add_error(result, name):
    types = set(filter(None, result["error_types"].split(";")))
    types.add(name)
    result["error_types"] = ";".join(sorted(types))


def _exit_on_signal(signum, frame):
    # Python's default SIGTERM/SIGHUP handling kills the process without
    # running `finally`, which would leave bus search disabled. Raising
    # SystemExit unwinds normally so the restore block runs.
    raise SystemExit(128 + signum)


def main():
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, _exit_on_signal)
    args = parse_args()
    counts = sorted(set(args.counts))
    methods = [m for m in METHODS if m in args.methods]
    seed = args.seed if args.seed is not None else int.from_bytes(os.urandom(4), "big")
    rng = random.Random(seed)

    # Discovery happens once, outside the timed region. Both libraries read
    # the same sensors in the same (ROM ID) order.
    sensors = sorted(W1ThermSensor.get_available_sensors(), key=lambda s: s.id)
    if len(sensors) < max(counts):
        sys.exit(f"Found {len(sensors)} sensors but need {max(counts)}. "
                 "Check wiring and /sys/bus/w1/devices/.")
    paths = [str(s.sensorpath.parent) for s in sensors]

    # Never call bus.rescan(): it writes 1 to w1_master_search.
    bus = W1Bus(unit="C")
    if set(bus.sensors) != set(paths):
        sys.exit("w1-therm-api and w1thermsensor discovered different sensors:\n"
                 f"  only w1-therm-api: {sorted(set(bus.sensors) - set(paths))}\n"
                 f"  only w1thermsensor: {sorted(set(paths) - set(bus.sensors))}")
    w1ta = W1ThermApiRunner(bus)

    env = common_env()
    libs = {name: library_meta(name) for name in {METHODS[m][0] for m in methods}}
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    per_count = {
        n: {
            "resolution_bits": device_attr(sensors[:n], "resolution"),
            "conv_time_ms": device_attr(sensors[:n], "conv_time"),
            "bus_sensor_count": bus_sensor_count(sensors[:n]),
        }
        for n in counts
    }

    print(f"run {run_id}: kernel {env['kernel']}, {env['os']}")
    for meta in libs.values():
        print(f"  {meta['library']} {meta['library_version']} ({meta['library_source']})")
    print(f"methods: {', '.join(methods)}")
    print(f"seed {seed}, gap {args.gap}s")
    print(f"sensors (sorted): {', '.join(s.id for s in sensors[:max(counts)])}")

    masters = sorted({bus_master(s) for s in sensors})
    original_search = {m: read_attr(m / "w1_master_search") for m in masters}
    search_disabled = False

    def run_cell(label, n):
        """Run one (method, N) cell. Drains pending bulk results before and
        after, outside the timed region."""
        flags = []
        if bulk_pending(masters):
            drain(paths)
            flags.append("BulkPendingBeforeRun")
            print(f"WARNING: bulk result pending before {label} N={n}; drained",
                  file=sys.stderr)
        if label == "w1thermsensor-seq":
            result = run_w1thermsensor(sensors[:n])
        elif label == "w1-therm-api-seq":
            result = w1ta.sequential(paths[:n])
        else:
            result, readout_done = w1ta.bulk(paths[:n])
            drain(paths[n:] if readout_done else paths)
            if bulk_pending(masters):
                flags.append("BulkPendingAfterDrain")
                print(f"WARNING: bulk result still pending after {label} N={n}",
                      file=sys.stderr)
        for flag in flags:
            add_error(result, flag)
        return result

    # Open the CSV before touching the bus, so a schema mismatch exits
    # without leaving search disabled.
    f, writer = open_csv(CSV_PATH)
    try:
        if len(sensors) >= EXPECTED_SENSORS:
            for m in masters:
                err = write_sysfs(m / "w1_master_search", "0")
                if err:
                    sys.exit(f"Could not disable search on {m.name}: {err}\n"
                             "Needs passwordless sudo, or run the venv's python as root.")
                search_disabled = True
            if search_state(masters) != "0":
                sys.exit(f"Search not disabled (w1_master_search = {search_state(masters)}).")
            print(f"bus search disabled on {', '.join(m.name for m in masters)} "
                  f"(was {';'.join(original_search.values())})")
        else:
            print(f"{len(sensors)}/{EXPECTED_SENSORS} sensors found: "
                  "leaving kernel bus search on")
        search = search_state(masters)

        for trial in range(-args.warmup, args.trials):
            # Shuffle every (method, N) cell each trial so slow drift
            # (temperature, background load) doesn't line up with either.
            cells = [(label, n) for label in methods for n in counts]
            rng.shuffle(cells)
            rows = []
            for run_order, (label, n) in enumerate(cells):
                result = run_cell(label, n)
                library, method, read_path = METHODS[label]
                rows.append((list(METHODS).index(label), n, {
                    "run_id": run_id,
                    "timestamp_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                    "method": method,
                    "read_path": read_path,
                    "sensor_count": n,
                    "trial": trial,
                    "run_order": run_order,
                    "seed": seed,
                    "gap_s": args.gap,
                    "w1_master_search": search,
                    **per_count[n],
                    **libs[library],
                    **env,
                    **result,  # may override read_path
                }))
                if args.gap:
                    time.sleep(args.gap)
            if trial >= 0:
                # Written grouped by method (METHODS order), then N.
                rows.sort(key=lambda r: r[:2])
                writer.writerows(row for _, _, row in rows)
                f.flush()  # keep completed trials if the run is interrupted
            label = "warmup" if trial < 0 else f"trial {trial + 1}/{args.trials}"
            print(f"  {label} done")
    finally:
        f.close()
        # Restore even on Ctrl-C or error. Warn rather than exit here so a
        # restore failure can't mask the original exception.
        if search_disabled:
            for m, value in original_search.items():
                if value == "unknown":
                    continue
                err = write_sysfs(m / "w1_master_search", value)
                if err:
                    print(f"WARNING: could not restore search on {m.name} to {value}: {err}",
                          file=sys.stderr)
            print(f"bus search restored ({search_state(masters)})")

    print(f"Results appended to {CSV_PATH}")


if __name__ == "__main__":
    main()
