###Pre-Format Draft

> **Drafting conventions for this file**
> - Bullets only; prose gets written from these.
> - `[@key]` = entry already in references.bib. `[NEEDS BIB: ...]` = source from planning.md with no bib entry yet.
> - `[VERIFY]` = check against the primary source before it goes in the paper.
> - `[RESULT]` = placeholder that depends on benchmark data.

# rs1wire: An efficient rust 1-wire ecosystem for thermal sensors

## Abstract

> Write last. Skeleton of what it needs, in order:

- Context: DS18B20 1-Wire sensors are widely read on Linux single-board computers (SBCs) through the kernel w1 driver
- Problem (one sentence each):
  - No maintained Rust library consumes the kernel w1 interface; existing Rust crates bit-bang for bare-metal targets
  - Most-used Python library (w1thermsensor) reads sensors sequentially, ~one conversion time per sensor
  - Kernel bulk conversion exists but can return stale values; no library prevents that at the API level
- Contribution: rs1wire, a Rust crate with discovery, individual and bulk reads, configuration, typed errors, stale-safe bulk reads by construction
- Evaluation: Raspberry Pi 5, 10x DS18B20 on one bus, vs. w1thermsensor 2.3.0 and w1-therm-api, sensor counts 1/2/5/10, two Linux distributions
- Key numbers: `[RESULT]` single-sensor parity, multi-sensor speedup, error rate, CPU
- Reminder: IEEE template forbids symbols, special characters, footnotes, and math in the abstract (so "85 C" / "~750 ms" need wording care)

## Introduction

### Context
- 1-Wire: Maxim/Analog Devices bus, one data line plus ground, many devices addressed by 64-bit ROM ID [@Analog_Devices_1w]
- DS18B20 digital temperature sensor is the common 1-Wire device on hobbyist and lab SBCs (Raspberry Pi) `[NEEDS BIB: DS18B20 datasheet]`
- Linux ships a 1-Wire subsystem: `w1-gpio` bus master + `w1_therm` slave driver; results exposed as sysfs files under `/sys/bus/w1/devices/` `[NEEDS BIB: Source 27, kernel w1_therm docs]`
- Rust is increasingly adopted for embedded and systems work for memory safety without a runtime [@sharma2024rust] `[NEEDS BIB: Source 23, optional]`

1-Wire 

### Problem (what is failing, for whom)
- **Rust users on Linux SBCs**
  - Existing Rust 1-Wire crates (one-wire-bus, ds18b20) target `embedded-hal` and implement protocol timing in software `[NEEDS BIB: Source 19]`
  - Bit-banging from Linux userspace is exposed to scheduler preemption and jitter [@adam2021performance]
  - The one Rust crate that reads kernel output, `w1_therm_reader`, is a ~99-line single-sensor parser, unchanged since v0.1.0 (2019), no discovery/bulk/config, panics on malformed input `[NEEDS BIB: Source 25]`
- **Python users**
  - w1thermsensor (packaged in Raspberry Pi OS) reads sensors one at a time in released versions `[NEEDS BIB: Source 21]`
  - Cost: ~750 ms (12-bit conversion) per sensor, so N sensors cost ~N conversion times
  - Users have asked for bulk read (w1thermsensor issue #115); PR #120 implements it but is unmerged `[NEEDS BIB: Source 31]`
- **Everyone using bulk conversion**
  - Kernel `therm_bulk_read` converts all sensors on a bus in ~one conversion time `[NEEDS BIB: Source 27]`
  - Hazard: a sensor not read promptly afterwards returns its trigger-time value on its next read, with no expiry `[NEEDS BIB: Source 28, kernel ABI doc]`
  - Existing bulk consumers (w1-therm-api, wb-mqtt-w1) do not prevent this at the API level

### Why it matters
- Multi-sensor setups (greenhouses, fermentation, server racks, lab rigs) scale linearly in read latency under sequential reads
- Silent stale or wrong values are worse than errors for monitoring/control loops
- Generic failure values (w1-therm-api returns `None` for every failure) hide the cause: wiring vs. CRC vs. power-on reset vs. missing driver

### Scope / boundary
- Hardware: Raspberry Pi 5, DS18B20 only (DS2413 only as an optional overhead probe, not a supported device)
- Software: Linux kernel w1 sysfs interface; no userspace 1-Wire timing
- Kernel tested: `6.18.50+rpt-rpi-2712` (Raspberry Pi OS, Debian 13 trixie) `[VERIFY at time of runs]`; second distribution `[TBD]`

### Contributions (bulleted list in the paper too)
1. rs1wire: Rust crate for the kernel `w1_therm` sysfs interface with discovery, individual reads, bulk reads, resolution/conv_time configuration
2. Bulk-read API that refuses stale reads by construction (double read, expired window, sensor not in conversion, pending result before an individual read)
3. Typed error taxonomy with a distinct variant per failure cause; parser never panics
4. Independent CRC-8 recomputation cross-checked against the kernel's verdict on every read path
5. Reproducible benchmark that separates the architecture effect (bulk vs. sequential) from the language/library effect (Rust vs. Python)
6. (Stretch) PyO3 Python bindings with the GIL released during blocking reads `[only if built]`

### What NOT to claim (keep handy while writing)
- Not first to consume w1 sysfs (w1thermsensor, w1_therm_reader do)
- Not first to use `therm_bulk_read` (w1-therm-api, wb-mqtt-w1, w1thermsensor PR #120 do)
- Not immune to timing problems: kernel `w1-gpio` still bit-bangs, in kernel space
- Claims about w1thermsensor are scoped to released versions (2.3.0)

### Paper organization
- Sec. II Related Work, III Background, IV Methodology, V Results, VI Discussion, VII Conclusion

## Related Work

> Suggested framing: organize by *approach* (userspace bit-bang vs. kernel consumer, sequential vs. bulk), then by language. A comparison table at the end makes the gap obvious.

### Python

- **w1thermsensor** (Timo Furrer; PyPI 2.3.0, Sep 27 2023) `[NEEDS BIB: Source 21]`
  - De facto standard; packaged in Raspberry Pi OS
  - Reads `w1_slave` per sensor; independent blocking file I/O
  - No `therm_bulk_read` in any released version (verified by grepping tagged releases `[cite version tags checked]`)
  - Documented multi-sensor pattern is a sequential loop of `get_temperature()`
  - CRC checking implemented correctly; the gap is the missing bulk path, not carelessness
  - Ships `AsyncW1ThermSensor`; concurrency does not help on one bus (see Background, bus mutex)
- **w1thermsensor PR #120**, "Add bulk read temperature method" (opened Feb 13 2025, unmerged) `[NEEDS BIB: Source 31]`
  - Closes issue #115 (users requesting bulk read): evidence of demand
  - Author-reported: 5 sensors, 12-bit, ~4.02 s to ~0.90 s; a user reported 7 sensors ~4.8 s to ~0.8 s (Feb 2026)
  - Informal, unreviewed numbers: use as sanity check on expected effect size, not as a result
  - `[VERIFY]` recheck merge status before final draft
- **w1-therm-api** (troxel; PyPI) `[NEEDS BIB: Source 29]`
  - Genuine bulk read: `start_conversion()` writes `trigger`, `wait_for_conversion()` polls every 5 ms, `read_temperatures()` reads each `temperature` file
  - No freshness enforcement:
    - `read_temperatures()` without a trigger silently does sequential per-sensor conversions
    - Reading long after a trigger silently returns trigger-time values
    - Partial reads then `read_temperatures()` silently mix stale bulk and fresh values
  - Every failure (missing sensor, CRC, unreadable file, power-on value) returns `None`; trigger failures only printed
  - Falls back to `w1_slave` on invalid/`85000`; per kernel source that second access starts a fresh conversion, silently adding ~one conversion time
  - Treats every 85000 as power-on reset (discards genuine 85 C)
  - Requires Python 3.10+ in practice despite README claiming 3.7+ (PEP 604 annotations)
  - Small/new: 3 commits, 0 stars
- **Community workaround evidence** (informal) `[NEEDS BIB: Source 26, RPi forum]`
  - Users report ~1 s per sensor sequentially, suggest asyncio/threading
- **Python performance in systems/hardware code** (motivation for a compiled core)
  - Rust/C/C++ comparably fast, generally outperform Python [@bugden2022safety]
  - On ESP32, MicroPython is orders of magnitude slower than compiled languages (e.g. CRC-32 ~986 us vs ~3 us C vs ~6.6 us Rust) [@plauska2022performance]
  - Pure-Python reimplementations of C-backed routines: 6x to 17,000x slower [@ding2026stdlib] (arXiv preprint, label as such)
  - Caveat to state: for this workload the ~750 ms conversion dominates; language speed matters only in per-sensor overhead

### Rust

- **embedded-hal 1-Wire crates** (fuchsnj/one-wire-bus, fuchsnj/ds18b20, others on crates.io `onewire` keyword) `[NEEDS BIB: Sources 18, 19]`
  - Designed for bare-metal MCUs; implement 1-Wire timing by toggling GPIO with delays
  - Assume no OS preemption mid-pulse; that assumption breaks in Linux userspace
  - Rust embedded community frames 1-Wire as a bit-banging HAL problem (embedded-hal issue #54) `[NEEDS BIB: Source 20]`
- **w1_therm_reader** (gaetronik, v0.1.0, Sep 1 2019) `[NEEDS BIB: Source 25]`
  - Only Rust crate found that reads kernel `w1_slave`
  - `temp.parse().unwrap()`: malformed `t=` line panics despite `io::Result` return type
  - CRC check is only the kernel's `crc=YES` string; no recompute
  - README example references a nonexistent function
  - No discovery, bulk read, resolution/conv_time, 85 C handling; sysfs root hardcoded
  - Built on nom 5; unchanged since release, 0 stars
- **Rust in embedded generally**: value and open problems [@sharma2024rust]; ecosystem overview `[NEEDS BIB: Source 23]`

### Other languages / applications
- **wb-mqtt-w1** (Wiren Board, C++) `[NEEDS BIB: Source 30]`
  - Detects `therm_bulk_read` per bus master, uses bulk when supported
  - Application daemon (MQTT), not a reusable library

### Python bindings for Rust (only if stretch goal is built)
- PyO3 gives real speedups but leaves per-call FFI overhead (~0.14 ms/call plus constant base) `[NEEDS BIB: Source 5, Basso do Amaral et al. 2025]` `[VERIFY figure]`
- Speedup contingent on workload shape `[NEEDS BIB: Source 6]`

### Gap summary (candidate Table I)

| Library | Lang | Kernel sysfs | Discovery | Bulk | Stale-safe | Typed errors | Config | Maintained |
|---|---|---|---|---|---|---|---|---|
| w1thermsensor 2.3.0 | Py | yes | yes | no (PR #120 open) | n/a | yes (exceptions) `[VERIFY granularity]` | yes `[VERIFY]` | yes |
| w1-therm-api | Py | yes | yes | yes | no | no (`None`) | `[VERIFY]` | new |
| wb-mqtt-w1 | C++ | yes | yes | yes | `[VERIFY]` | n/a (app) | `[VERIFY]` | yes |
| one-wire-bus / ds18b20 | Rust | no (bit-bang) | yes | n/a | n/a | yes | yes | `[VERIFY]` |
| w1_therm_reader | Rust | yes | no | no | n/a | no (panics) | no | no |
| **rs1wire** | Rust | yes | yes | yes | **yes** | **yes** | yes | yes |

## Background

### 1-Wire protocol [@Analog_Devices_1w] [@Analog_Devices_1w_software]
- Single data line + ground; open-drain with one pull-up resistor (4.7 kOhm typical) to VCC
- Master-initiated; every transaction starts with reset pulse and device presence pulse
- Bits sent in time slots on the order of tens of microseconds; master and slaves sample at fixed offsets, so timing errors corrupt bits
- Each device has a unique 64-bit ROM ID: 8-bit family code, 48-bit serial, 8-bit CRC
- Search ROM algorithm discovers all devices on a bus
- Power: external VCC, or parasite power from the data line (needs a strong pull-up during conversion)
- Software (bit-banged) masters are common; the timing requirement is the hard part [@Analog_Devices_1w_software]

### DS18B20 `[NEEDS BIB: DS18B20 datasheet]`
- Family code `0x28`
- Range -55 to +125 C; +/-0.5 C accuracy from -10 to +85 C `[VERIFY]`
- Configurable resolution 9-12 bits; max conversion time 93.75 / 187.5 / 375 / 750 ms
- Convert T command starts conversion; result read from 9-byte scratchpad
- Scratchpad byte 8 is Dallas/Maxim CRC-8 over bytes 0-7 (polynomial x^8 + x^5 + x^4 + 1)
- Power-on reset value of the temperature register is +85 C; indistinguishable from a genuine 85 C reading
- Resolution lives in scratchpad RAM (lost on power off unless copied to EEPROM)
- Convert T can be addressed to all devices at once (Skip ROM), which is what makes bulk conversion possible

### Linux w1 subsystem `[NEEDS BIB: Source 27, Source 28]`
- `w1-gpio`: bus master driver that bit-bangs a GPIO pin from kernel space
  - Enabled on Raspberry Pi via `dtoverlay=w1-gpio` in `/boot/firmware/config.txt` (default GPIO4; `gpiopin=` to change) `[VERIFY + cite RPi config docs]`
- `w1_therm`: slave driver for temperature sensors
- sysfs layout:
  - `/sys/bus/w1/devices/w1_bus_masterN/` with `w1_master_slaves`, `w1_master_slave_count`, `w1_master_search`, `therm_bulk_read`
  - `/sys/bus/w1/devices/28-xxxxxxxxxxxx/` (symlink into its bus master) with `w1_slave`, `temperature`, `resolution`, `conv_time`, others
- `w1_slave` format (two lines):
  - Line 1: 9 scratchpad bytes, `: crc=XX YES|NO`
  - Line 2: bytes of the last scratchpad that passed CRC (not necessarily the current one), then `t=<millidegrees C>`
- Reading `w1_slave` or `temperature` with no pending bulk result triggers a single-sensor conversion and blocks ~conv_time
- `therm_bulk_read` (per bus master):
  - Write `trigger`: Convert T to all sensors on the bus
  - Write is synchronous: kernel sleeps for the longest `conv_time` on the bus before returning (from `w1_therm.c`, rpi-6.18.y) `[VERIFY line refs]`
  - Read: `-1` conversion in progress, `1` complete with unread results, `0` nothing pending
  - Default permissions 0644 root: unprivileged users cannot trigger
- Stale-read mechanism (kernel source, rpi-6.18.y):
  - Each sensor has a pending-result flag set by the bulk trigger
  - `w1_slave_show` / `temperature_show`: if flag is 1, read scratchpad and clear it; if 0, call `convert_t()` (fresh conversion)
  - No expiry: a result read hours later is still the trigger-time value
  - Consequence 1: late read returns stale data silently
  - Consequence 2: second read of same sensor silently costs a new full conversion
- Read failures: kernel returns an empty file rather than an error code
- Bus mutex (rpi-6.18.y): without strong pull-up (externally powered), held through the conversion sleep, so concurrent per-sensor conversions on one bus serialize; with strong pull-up it is released before sleeping `[VERIFY on hardware: async vs. sequential timing]`
  - Implication: threads/asyncio cannot recover the bulk advantage on a single externally powered bus

### Userspace timing on Linux (why not bit-bang in userspace)
- Userspace worst-case GPIO latency ~147-160 us vs. kernel-space ~67-76 us on Pi 3 / BeagleBone [@adam2021performance] `[VERIFY figures]`
- PREEMPT_RT brings jitter below ~50 us `[NEEDS BIB: Source 9, Adam et al. MDPI Computers 2021]`
- Even RT Linux shows tens of us jitter; sub-us needs interrupt isolation `[NEEDS BIB: Source 11]` (preprint)
- Real-time tuning needs isolcpus, IRQ affinity, tickless operation `[NEEDS BIB: Source 10, NASA]`
- 1-Wire slots are tens of us, so these latencies are the same order as the slot timing
- Caveat to state: kernel `w1-gpio` also bit-bangs; kernel space reduces but does not eliminate the problem, and rs1wire inherits it

### Raspberry Pi 5 / RP1 specifics (primary sources only, say so)
- GPIO moved off the SoC to the RP1 I/O controller over PCIe; inherently higher latency than on-die GPIO `[NEEDS BIB: Source 13, Upton 2023]`
- Most PIOLib operations take at least 10 us `[NEEDS BIB: Source 14, Elwell 2024]`
- Direct register access libraries (pigpio, RPi.GPIO) broke on Pi 5 `[NEEDS BIB: Source 17]`
- RP1 registers proxied via firmware mailbox in some kernel paths `[NEEDS BIB: Source 16]` `[VERIFY context]`
- Earlier w1-gpio instability on Pi 5 fixed from kernel 6.6.28+ `[VERIFY: replace forum source with kernel commit/changelog]`
- `[VERIFY]` ~1 us direct-write figure from RP1 datasheet before quoting
- Open note: Raspberry Pi docs inconsistent on PCIe generation (Gen 2 x4 vs. Gen 3); avoid stating it or cite carefully

## Methodolgy

> Design and harness are implemented, so these bullets reflect the current code. Hardware figures already placed in rs1wire.tex.

### Design goals
- Consume only kernel-produced results; never implement 1-Wire timing in userspace
- Make stale bulk reads impossible to do by accident (type system + runtime checks)
- One error variant per failure cause; never panic on kernel output
- Same validation on every read path

### API structure (Rust)
- `W1`: sysfs root (default `/sys/bus/w1/devices`, overridable for fixture tests)
  - `bus_masters()`, `bus(name)`, `sensors()`, `sensor(id)`
- `BusMaster`: `sensors()` (from `w1_master_slaves`, DS18B20 only, foreign families skipped), `bulk_status()`, `bulk_convert(opts)`, `discard_pending()`
- `Sensor`: `read()` / `read_with(opts)`, `resolution()` / `set_resolution()`, `conv_time()` / `set_conv_time()`
- Value types: `DeviceId` (family + 6-byte serial), `Temperature` (millidegrees C, integer), `Resolution` (9-12 bit enum), `ConvTime` (`DEFAULT`, `MEASURE`, or millis >= 2)

### Stale-read prevention (core contribution; candidate figure: state diagram)
<!-- TODO: Discuss the initial transition in Fig. bulk_states(b), labeled "w1 driver loaded": therm_bulk_read only exists once the bus master driver (w1-gpio + w1-therm) is loaded, and it starts at 0 (Idle) with nothing pending. Tie to setup: overlay configured in /boot/firmware/config.txt (dtoverlay=w1-gpio,gpiopin=N). Keep brief; no new section header (subheadings OK). -->
- `bulk_convert()` returns a `BulkConversion` that mutably borrows the `BusMaster`
  - Compile time: at most one live conversion per bus handle
  - `read_all()` consumes the conversion: reading all twice does not compile (verified with a `compile_fail` doctest)
- Runtime refusals (`Error::StaleBulkRead` with `StaleReason`):
  - `AlreadyRead`: second read of a sensor would silently start a fresh conversion
  - `Expired`: read after `max_age` (default 1 s, crate-enforced since kernel has no expiry)
  - `NotInConversion`: sensor not on the bus at trigger time
  - `BulkPending`: individual `Sensor::read` refused while bus has unread bulk results (would return them instead of converting)
- `Drop` reads and discards every result not handed out (~10-15 ms each), so leftovers cannot leak into a later individual read
- Pre-trigger check: refuse with `ConversionInProgress` if `therm_bulk_read` is -1
- Known limitation: check-then-read races with other processes that trigger in between (documented)

### Integrity checks
- CRC: recompute Dallas/Maxim CRC-8 over scratchpad bytes 0-7; error if it differs from byte 8, if kernel says `NO`, or if kernel's printed CRC differs from the recomputed one (`CrcMismatch`)
- Bulk results also read through `w1_slave`, so CRC is recomputed on every path (differs from w1-therm-api, which reads `temperature`)
- Empty `w1_slave` -> `ReadFailed` (kernel's way of signalling a failed read)
- All-zero scratchpad -> `ReadFailed` (passes CRC-8 but means nothing drove the bus)
- 85 C: `PowerOnResetPolicy::Reject` by default (`PowerOnReset` error); opt-in `Accept` for applications that measure 85 C. Trade-off stated explicitly, unlike w1-therm-api's silent discard

### Error taxonomy (candidate Table II)
- `ModuleNotLoaded { w1-gpio | w1-therm }`, `DeviceNotFound`, `BusNotFound`, `BulkReadUnsupported` (no file or `ENODEV` on trigger), `CrcMismatch`, `ConversionInProgress`, `StaleBulkRead { reason }`, `ReadFailed`, `Parse { path, kind }`, `PowerOnReset`, `Io { path, source }`
- Enum is `#[non_exhaustive]`; parse errors carry a truncated snippet of the offending input

### Parser
- Pure functions, no I/O; strict match to kernel format (exactly 9 two-digit hex bytes, `YES|NO`, `%d` integers)
- Rejects `+` signs, overflow, extra lines, wrong byte counts; returns errors, never panics

### Correctness testing
- Fixture sysfs trees in temp dirs (plain files; cannot model kernel consuming a bulk result)
- Coverage: discovery (multiple buses, empty bus, foreign family), malformed `w1_slave`, CRC failure variants, empty read, all-zero scratchpad, power-on reset policy, missing driver vs. missing device, bulk double read / expired / not-in-conversion, bulk unsupported, configuration read/write
- Kernel-side consumption behaviour checked on hardware with `examples/smoke.rs`
- Error rate also recorded during benchmark runs

### Hardware setup (figures in .tex)
- Raspberry Pi 5 (1 GB), 10x DS18B20 waterproof probes on one `w1-gpio` bus, externally powered (not parasite) `[VERIFY]`
- Single 4.7 kOhm pull-up `[VERIFY: measured DATA-VCC resistance after removing parallel adapter-board pull-ups]`
- DS2413 on a separate GPIO / overlay instance, isolated (only if Regime 3 runs)
- Resolution 12-bit, kernel default conv_time `[VERIFY from CSV columns]`

### Software environment
- OS 1: Raspberry Pi OS (Debian 13 trixie), kernel `6.18.50+rpt-rpi-2712`
- OS 2: `[TBD distribution]`
- Python baselines: w1thermsensor 2.3.0, w1-therm-api `[version]`, Python `[version]`; rustc `[version]`, release build
- All recorded per row in `timings.csv` (kernel, os_id, os_version, library versions + source commit, resolution, conv_time)

### Benchmark methods (same CSV schema for Python and Rust harnesses)
- `w1thermsensor-seq`: `get_temperature()` per sensor (`w1_slave`)
- `w1-therm-api-seq`: `read_temperatures()` with no trigger (separates library effect from bulk effect)
- `w1-therm-api-bulk`: `start_conversion()` + `wait_for_conversion()` + `read_temperatures()`
- `rs1wire-seq`: `Sensor::read()` per sensor
- `rs1wire-bulk`: `bulk_convert()` + `BulkConversion::read()` per sensor
- Optional: w1thermsensor async, PR #120 branch (commit hash recorded), rs1wire via Python bindings

### Comparisons each pairing isolates
- w1thermsensor-seq vs. w1-therm-api-bulk / rs1wire-bulk: architecture (bulk vs. sequential)
- w1-therm-api-seq vs. w1-therm-api-bulk: architecture within one codebase
- w1-therm-api-bulk vs. rs1wire-bulk: language/library effect
- w1thermsensor-seq vs. w1-therm-api-seq vs. rs1wire-seq: per-call overhead across libraries
- Async vs. bulk (if run): does concurrency close the gap (expected no, given bus mutex)

### Experimental protocol
- Sensor counts N = 1, 2, 5, 10, fixed in advance; first N sensors by ROM ID, same order in both harnesses
- Each trial runs every (method, N) cell in a seeded random order; seed and `run_order` recorded
- 1 unrecorded warmup trial
- `run_all.sh` runs blocks py, rs, rs, py with 15 trials each (30 per library) so slow drift doesn't align with one library
- Bulk triggers convert every sensor on the bus: after each bulk cell, unread sensors are drained (untimed); `therm_bulk_read` checked before every cell; violations flagged `BulkPendingBeforeRun` / `BulkPendingAfterDrain`
- Kernel periodic bus search disabled (`w1_master_search = 0`) with full bus present, restored on exit, so search cycles can't hold the bus mutex mid-trial
- `therm_bulk_read` made writable for the `gpio` group via udev rule; benchmarks run unprivileged
- Pre-run checks: Cargo.lock committed, clean tree (no `-dirty` source), trigger writable, sudo for search toggle

### Metrics
- Wall-clock (`perf_counter` / `Instant`)
- CPU total (`process_time` / `CLOCK_PROCESS_CPUTIME_ID`); user/sys split from `getrusage` (tick-sampled, approximate)
- Phase timings where possible: trigger, wait, readout
  - Note: kernel trigger write is synchronous, so w1-therm-api's `trigger_s` holds the whole conversion and `wait_s` ~0; rs1wire leaves `wait_s` empty `[decide labeling before results]`
- Reads ok / failed, error types
- CPU caveat: w1-therm-api's 5 ms polling loop counts toward its CPU

### Expected outcomes (pre-stated targets, from project definition)
- N = 1: parity across all libraries (conversion dominates)
- N > 1: rs1wire-bulk clearly faster than w1thermsensor-seq; at least competitive with w1-therm-api-bulk
- Sanity check: PR #120 reports ~4.5x for 5 sensors

### Threats to validity
- Rust and Python harnesses run as separate processes (not interleaved in one shuffle); mitigated by py/rs/rs/py block order
- Single board, single bus, max 10 sensors; results may not generalize to larger buses or other masters (e.g. DS2482 I2C bridges)
- Kernel version dependence (bulk semantics, mutex behaviour)
- Room temperature drift across runs

## Results 

> Depends on data. Planned structure only.

- `[RESULT]` Table: median (IQR) wall time per method x N, per distribution
- `[RESULT]` Figure: wall time vs. N, one line per method (shows linear vs. flat scaling)
- `[RESULT]` Figure or table: CPU time per method x N
- `[RESULT]` Phase breakdown for bulk methods (trigger/conversion vs. readout)
- `[RESULT]` Error counts by type, per method; any `BulkPending*` flags
- `[RESULT]` Distribution comparison (same board, OS 1 vs. OS 2)
- `[RESULT]` Correctness: test suite pass count; stale-read refusal demonstrations
- `[RESULT]` (if run) async and PR #120 baselines; (if built) Python bindings

## Discussion

> Interpretation needs results; these caveats can be written now.

- Where speed matters vs. doesn't: single sensor is conversion-bound; gains come from architecture (bulk) and per-sensor overhead
- Separate how much of any gain is architecture vs. language; don't let Rust take credit for bulk
- Stale-read safety has a cost: drain on drop (~10-15 ms per unread sensor), freshness window is a policy choice not a physical limit
- 85 C default rejects genuine readings; trade-off vs. silent wrong values
- Kernel bit-banging is not immune to jitter; rs1wire inherits kernel bugs and the RP1 latency characteristics
- Concurrency: whether async closes the gap (`[RESULT]`), explained by bus mutex behaviour
- Bulk trigger needs write access to a root-owned file; deployment requires a udev rule or root
- Race between status check and read with other processes; cannot be closed from userspace
- RP1 latency figures are primary-source (vendor), not peer-reviewed
- If PyO3 bindings built: FFI overhead is not zero `[NEEDS BIB: Source 5]`

## Conclusions

- Restate gap: no maintained Rust kernel-w1 library; no library preventing stale bulk reads
- Restate contributions (match Introduction list)
- Headline numbers `[RESULT]`
- Future work:
  - Other 1-Wire families (DS2413, DS2438, etc.)
  - Other bus masters (DS2482 I2C bridge) and boards
  - Python bindings (if not completed) and publication to crates.io / PyPI
  - Upstreaming bulk support or freshness guidance to w1thermsensor
  - Parasite-power configurations (strong pull-up changes mutex behaviour)

## References

> Status of sources cited above.

### Already in references.bib
- `adam2021performance` (Source 8)
- `plauska2022performance` (Source 2) — bib says 2022, planning.md says 2023; vol. 12 no. 1 was published Dec 2022, so 2022 is likely right `[VERIFY]`
- `bugden2022safety` (Source 1)
- `ding2026stdlib` (Source 3, preprint)
- `sharma2024rust` (Source 22)
- `Analog_Devices_1w`, `Analog_Devices_1w_software`
- `IEEE_Computational_Intelligence_Society_2024` (template; probably remove before submission)

### Needs a bib entry (priority order)
1. Linux kernel docs, w1_therm driver (Source 27)
2. Linux kernel sysfs ABI, sysfs-driver-w1_therm (Source 28)
3. DS18B20 datasheet (Analog Devices/Maxim)
4. w1thermsensor (Source 21)
5. w1-therm-api (Source 29)
6. w1_therm_reader (Source 25)
7. w1thermsensor PR #120 (Source 31)
8. fuchsnj one-wire-bus / ds18b20 (Source 19), crates.io onewire listing (Source 18)
9. Adam et al., MDPI Computers 2021 (Source 9)
10. Upton RP1 post (Source 13), Elwell PIOLib post (Source 14)
11. wb-mqtt-w1 (Source 30)
12. Raspberry Pi config.txt / overlay docs (for `dtoverlay=w1-gpio`)
13. Optional: Sources 5, 6, 10, 11, 16, 17, 20, 23, 26