# CLAUDE.md

## Project

Rust crate (working name: rs1wire) that reads 1-Wire devices through the Linux kernel's w1 sysfs interface at `/sys/bus/w1/devices/`, with Python bindings via PyO3/maturin. This is a capstone project for a technical project development class, so correctness, verifiable claims, and honest benchmarking matter more than feature count.

`docs/project-definition.md` is the authoritative definition of the project's scope (problem, objectives, minimum scope, non-goals). `docs/planning.md` holds supporting research, sources, and working notes. Read both before making design decisions. If a decision here changes something in planning.md, update it and add a dated line to its Changelog section.

Python bindings are a stretch goal, not part of the minimum scope.

## Target environment

- Raspberry Pi 5 running Raspberry Pi OS with the `w1-gpio` and `w1-therm` kernel modules (exact OS/kernel versions: TBD, record them here once fixed), plus at least one other Linux distribution on the same board (TBD)
- Sensors: up to 10 DS18B20s (thermal) on one bus; DS2413 (non-thermal) on a separate GPIO pin and w1-gpio overlay instance, used only to measure per-call overhead in benchmarks, not a supported device
- Known risk: a Pi 5/RP1 w1-gpio instability bug may exist on some kernels. Plan is to pin a kernel version where it's resolved.

## What this crate is (and isn't)

- It consumes the kernel's already-produced results. It never bit-bangs 1-Wire timing in userspace.
- The contribution is NOT "consuming sysfs" or "using bulk read"; prior art exists for both (w1thermsensor, w1_therm_reader, w1-therm-api, wb-mqtt-w1, and an unmerged w1thermsensor PR: https://github.com/timofurrer/w1thermsensor/pull/120).
- The contribution IS: discovery + bulk conversion + configuration in a maintained Rust crate, a bulk-read API that cannot return stale data by construction, typed and distinct errors instead of a generic failure value, and (as a stretch goal) Python bindings that release the GIL.

## Scope

- **Minimum:** discovery, individual reads, stale-safe bulk reads, typed errors.
- **Beyond minimum:** resolution/conv_time configuration.
- **Stretch:** Python bindings.
- **Non-goals:** do not add support for non-temperature 1-Wire devices; do not implement 1-Wire timing in userspace; do not add hardware-specific code for boards other than the Pi 5.

## Design rules

- **Never panic on device or sysfs input.** Malformed `w1_slave` or `temperature` content returns an error. No `unwrap()`/`expect()` on parsed data outside tests.
- **Stale-read safety.** After a `therm_bulk_read` trigger, the kernel returns the value from trigger time if a sensor isn't read promptly. The bulk-read API must make stale access impossible or an explicit error (e.g. a result/handle type consumed by reading, tied to one trigger). Tests must show that an expired conversion and a double read from one conversion are both rejected.
- **Error taxonomy** (keep these distinct): kernel module not loaded, device not found, CRC mismatch, conversion in progress, stale bulk-read access, parse failure, 85 °C power-on reset value.
- **CRC:** decision pending between recomputing CRC-8 from the 9 raw scratchpad bytes and only checking the kernel's `crc=YES/NO`. Docs must describe whichever is chosen accurately.
- **85 °C handling:** decision pending on whether `t=85000` is always treated as the power-on reset value, since that discards genuine 85 °C readings. Document whichever is chosen.
- **Testability:** the sysfs root path must be configurable so tests run against fixture directories, not real hardware.
- **Python bindings:** release the GIL (`py.allow_threads`) around every blocking file read and conversion wait.

## Kernel interface reference

- Per-device: `/sys/bus/w1/devices/<family>-<id>/w1_slave` (two-line hex + `crc=` + `t=` in m°C) and `temperature`
- Per-master: `/sys/bus/w1/devices/w1_bus_masterN/therm_bulk_read`
  - write `trigger` to start a conversion on all devices on that bus
  - read: `-1` conversion in progress, `1` complete but some sensors unread, `0` no bulk operation pending
- Kernel docs: https://docs.kernel.org/w1/slaves/w1_therm.html and `Documentation/ABI/testing/sysfs-driver-w1_therm`

## Benchmarking

The benchmark harness exists to support specific claims, so don't change regimes or baselines without updating `docs/planning.md`.

- Regime 1: single sensor, normal polling (expect conversion time to dominate; don't oversell)
- Regime 2: many sensors at fixed counts within 1 to 10 (e.g. 1, 2, 5, 10). Required baselines: w1thermsensor 2.3.0 (sequential), w1-therm-api (bulk), this crate native. Optional: w1thermsensor async, w1thermsensor PR #120 branch (record commit hash), this crate via Python if bindings exist.
- Regime 3 (optional): DS2413, no conversion delay, isolates per-call overhead
- Metrics: wall-clock time, CPU usage, error rate
- w1-therm-api must be called as `convert()` then `read_temperatures()`, with `unit="C"`. Log which read path was used and count None/error results. Time the trigger, wait, and readout phases separately where possible.
- Record kernel version, OS version, Python version, library versions, sensor count, and resolution with every result

## Style

- Keep public API docs honest about limitations (the kernel's w1-gpio driver still bit-bangs in kernel space; this crate inherits its jitter and bugs).