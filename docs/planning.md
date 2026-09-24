# rs1wire Crate Notes & Planning

David Costa | Created 9/16/2026 | Last updated 9/24/2026

## Project definition

`docs/project-definition.md` is the authoritative problem statement, objectives, evaluation criteria, minimum scope, and non-goals for this project. This file holds supporting research, sources, and working notes; where the two disagree, project-definition.md wins.

- **Minimum scope:** discovery, individual reads, stale-safe bulk reads with typed errors, tested on Raspberry Pi OS, benchmarked against w1thermsensor 2.3.0 and w1-therm-api.
- **Beyond minimum:** sensor configuration (resolution, conv_time), Python bindings (stretch goal), second Linux distribution on the same Pi 5.
- **Non-goals:** no 1-Wire device types beyond temperature sensors (DS2413 used only to measure per-call overhead in benchmarks); no hardware beyond Raspberry Pi 5, DS18B20s, and the DS2413; no 1-Wire timing in userspace.

## Notes for writing/defense

Strong Problem Statement:

- Who/where
- What is failing?
- Why does it matter?
- Boundary/Scope?

Evidence **MUST** prove the objective.

Credible evidence, proportionate claim.

Claims/results must be verifiable.

Objective must be specific, ie:

- DON'T: Package for Raspberry Pi sensor reading
- DO: Rust and Python Package for Raspberry Pi 5 1-Wire sensor reading via Linux drivers (should also include required versions of Linux/Raspberry Pi OS).

## Contribution statement (working draft)

What is **not** new, and should not be claimed:

- Consuming the kernel's w1 sysfs interface. w1thermsensor (Python - https://pypi.org/project/w1thermsensor/) does this via `w1_slave`, and w1_therm_reader (Rust - https://github.com/gaetronik/w1_therm_reader) does it (poorly) too.
- Using `therm_bulk_read`. w1-therm-api (Python) and wb-mqtt-w1 (C++) already use it. An unmerged community PR against w1thermsensor also implements it: https://github.com/timofurrer/w1thermsensor/pull/120

What **is** new:

- A maintained Rust crate for the w1_therm sysfs interface with device discovery, bulk conversion, and sensor configuration (resolution, conv_time).
- A bulk-read API that prevents stale reads by construction (the kernel returns the value from bulk-trigger time if a sensor isn't read immediately).
- Typed, distinct errors instead of a generic failure value (w1-therm-api returns None for every failure type).
- Python bindings via PyO3/maturin with the GIL released during blocking reads (stretch goal).
- A benchmark that separates the architectural win (bulk vs. sequential) from the language/bindings win (Rust vs. Python).

## Claims & Supporting Sources

### Claim 1: Python has real overhead/slowness in hardware and systems-level code

- **Source 1:** Bugden & Alahmar, "The Safety and Performance of Prominent Programming Languages" (2022). [https://arxiv.org/pdf/2206.05503](https://arxiv.org/pdf/2206.05503) Peer-reviewed (IJSEKE). Six-language benchmark; found Rust/C/C++ comparably fast and generally outperforming Python.
- **Source 2:** Plauska, Liutkevičius & Janavičiūtė, "Performance Evaluation of C/C++, MicroPython, Rust and TinyGo on ESP32" (2023). [https://www.mdpi.com/2079-9292/12/1/143](https://www.mdpi.com/2079-9292/12/1/143) Peer-reviewed (MDPI Electronics). MicroPython execution times "thousands of times worse" than compiled languages on real embedded hardware (e.g. CRC-32: \~986µs vs \~3µs C vs \~6.6µs Rust).
- **Source 3:** "Stdlib or Third-Party? Empirical Performance and Correctness of LLM-Assisted Zero-Dependency Python Libraries" (2026). [https://arxiv.org/pdf/2605.21405](https://arxiv.org/pdf/2605.21405) arXiv preprint. Documents a "C-extension performance cliff": pure-Python reimplementations of normally-C-backed routines run 6x to 17,000x slower.
- **Source 4:** Codeandlife, "Benchmarking Raspberry Pi GPIO Speed". [https://codeandlife.com/2012/07/03/benchmarking-raspberry-pi-gpio-speed/](https://codeandlife.com/2012/07/03/benchmarking-raspberry-pi-gpio-speed/) Informal blog, not citable academically. GPIO toggling: \~22MHz in C vs \~70kHz in Python. Good for motivation only.

### Claim 2: Rust via PyO3 gives genuine speedups but doesn't eliminate FFI overhead

- **Source 5:** Basso do Amaral, Ferreira & Goldman (USP), "Rust vs. C for Python Libraries: Evaluating Rust-Compatible Bindings Toolchains" (2025). [https://arxiv.org/abs/2507.00264](https://arxiv.org/abs/2507.00264) arXiv preprint, most on-point paper found. Quantifies PyO3's residual per-call overhead (\~0.14ms/call plus constant base). Centerpiece citation for this claim.
- **Source 6:** "Improving Runtime Performance of Tensor Computations using Rust" (2025). [https://arxiv.org/html/2510.01495v1](https://arxiv.org/html/2510.01495v1) arXiv preprint. Discusses PyO3 FFI overhead vs NumPy; speedup is real but contingent on several factors.
- **Source 7:** "Towards Reliable Memory Management for Python Native Extensions" (CyStck), ICOOOLPS workshop (2023). [https://conf.researchr.org/details/ecoop-issta-2023/ICOOOLPS-2023/4/](https://conf.researchr.org/details/ecoop-issta-2023/ICOOOLPS-2023/4/) Peer-reviewed workshop paper. Analyzes Python C-API/FFI overhead generally.

### Claim 3: Userspace bit-banging on general-purpose Linux faces scheduler preemption/jitter that kernel-space or hardware-assisted approaches reduce

- **Source 8:** "Performance Assessment of Linux Kernels with PREEMPT_RT on ARM-Based SBCs" (2021). [https://www.mdpi.com/2079-9292/10/11/1331](https://www.mdpi.com/2079-9292/10/11/1331) Peer-reviewed (MDPI Electronics). Measured on Pi 3/BeagleBone: userspace worst-case GPIO latency \~147–160µs vs kernel-space \~67–76µs.
- **Source 9:** "Real-Time Performance and Response Latency Measurements of Linux Kernels on SBCs" (2021). [https://www.mdpi.com/2073-431X/10/5/64](https://www.mdpi.com/2073-431X/10/5/64) Peer-reviewed (MDPI Computers). Companion study; PREEMPT_RT drops jitter below 50µs. Dataset: [https://github.com/gadam2018/RPi-BeagleBone](https://github.com/gadam2018/RPi-BeagleBone)
- **Source 10:** NASA, "Challenges Using Linux as a Real-Time Operating System" (2020). [https://ntrs.nasa.gov/api/citations/20200002390/downloads/20200002390.pdf](https://ntrs.nasa.gov/api/citations/20200002390/downloads/20200002390.pdf) Reputable technical report. Covers isolcpus, IRQ affinity, disabling timer tick.
- **Source 11:** Zhou et al., "Towards Deterministic Sub-0.5µs Response on Linux through Interrupt Isolation" (2025). [https://arxiv.org/pdf/2509.03855](https://arxiv.org/pdf/2509.03855) arXiv preprint. Quantifies standard/RT Linux jitter (up to 72µs / 67.9µs) vs interrupt isolation (\<470ns).
- **Source 12:** "CANflict: Exploiting Peripheral Conflicts..." (2022). [https://arxiv.org/pdf/2209.09557](https://arxiv.org/pdf/2209.09557) arXiv preprint. Hardware peripherals performed 5-10x better than basic bit-banging in their tests. (Also relevant to hardware-vs-software tradeoff generally.)

### Claim 4: Raspberry Pi 5's RP1 architecture introduces GPIO timing challenges earlier Pis didn't have

*(No peer-reviewed literature exists for this claim. Primary/official documentation only.)*

- **Source 13:** Eben Upton, "RP1: the silicon controlling Raspberry Pi 5 I/O" (2023). [https://www.raspberrypi.com/news/rp1-the-silicon-controlling-raspberry-pi-5-i-o-designed-here-at-raspberry-pi/](https://www.raspberrypi.com/news/rp1-the-silicon-controlling-raspberry-pi-5-i-o-designed-here-at-raspberry-pi/) Official primary source. States the RP1 link is inherently higher-latency than on-die GPIO.
- **Source 14:** Phil Elwell, "PIOLib: A userspace library for PIO control" (2024). [https://www.raspberrypi.com/news/piolib-a-userspace-library-for-pio-control/](https://www.raspberrypi.com/news/piolib-a-userspace-library-for-pio-control/) Official primary source, headline figure: "most PIOLib operations take at least 10 microseconds." Best quotable number for this claim.
- **Source 15:** RP1 Peripherals Datasheet. [https://datasheets.raspberrypi.com/rp1/rp1-peripherals.pdf](https://datasheets.raspberrypi.com/rp1/rp1-peripherals.pdf) Official datasheet. A \~1µs direct-write figure is reported third-party (unverified directly). Check wording yourself before quoting.
- **Source 16:** PR #6470, raspberrypi/linux. [https://github.com/raspberrypi/linux/pull/6470](https://github.com/raspberrypi/linux/pull/6470) Kernel driver author confirms RP1 registers are proxied via firmware mailbox, not directly memory-mapped.
- **Source 17:** pigpio issue #589. [https://github.com/joan2937/pigpio/issues/589](https://github.com/joan2937/pigpio/issues/589) Confirms pigpio (and RPi.GPIO) are broken on Pi 5 because DMA/register access moved off-die.

### Claim 5 (REVISED 9/23): The only Rust crate that consumes the w1_therm sysfs interface is a minimal, unmaintained single-sensor parser. No Rust crate supports device discovery, the bulk-conversion interface, or sensor configuration, and none provides Python bindings.

*(No academic literature. Primary ecosystem evidence only.)*

> **Why revised:** The original wording ("none consume kernel sysfs") is false. `w1_therm_reader` (Source 25) is published on crates.io and reads `/sys/bus/w1/devices/{id}/w1_slave`. The gap is real but narrower: it's about capability and maintenance, not about sysfs consumption existing at all.

- **Source 18:** crates.io "onewire" keyword listing. [https://crates.io/keywords/onewire](https://crates.io/keywords/onewire) Most Rust 1-Wire crates target embedded-hal (bit-banging). **Needs recheck:** w1_therm_reader ranks #7 in #onewire on lib.rs and may appear in this listing too. Don't cite this source as "none consume sysfs."
- **Source 19:** fuchsnj/one-wire-bus and fuchsnj/ds18b20. [https://github.com/fuchsnj/one-wire-bus](https://github.com/fuchsnj/one-wire-bus) / [https://github.com/fuchsnj/ds18b20](https://github.com/fuchsnj/ds18b20) The prominent Rust 1-Wire crates; confirmed embedded-hal-based, software-timed.
- **Source 20:** embedded-hal issue #54. [https://github.com/rust-embedded/embedded-hal/issues/54](https://github.com/rust-embedded/embedded-hal/issues/54) Confirms the Rust embedded community frames 1-Wire as a bit-banging problem, not a kernel-consumer problem.
- **Source 21:** timofurrer/w1thermsensor. [https://github.com/timofurrer/w1thermsensor](https://github.com/timofurrer/w1thermsensor) (PyPI 2.3.0, released Sep 27, 2023) The Python baseline library. **Does** consume sysfs `w1_slave` (that's its whole mechanism). No released version (through 2.3.0, Sep 2023) uses `therm_bulk_read`; multi-sensor usage in released versions is a sequential loop of per-sensor `get_temperature()` calls. An unmerged community PR adds a synchronous bulk-read method (see Source 31). Also ships `AsyncW1ThermSensor`, which may allow concurrent per-sensor reads (see Working Notes).
- **Source 25 (NEW):** gaetronik/w1_therm_reader. [https://github.com/gaetronik/w1_therm_reader](https://github.com/gaetronik/w1_therm_reader) / [https://docs.rs/w1_therm_reader](https://docs.rs/w1_therm_reader) The only Rust crate found that consumes w1_therm sysfs. v0.1.0 (Sep 1, 2019), \~99 lines, 0 stars, unchanged since release. Weaknesses confirmed from source:
  - Parser calls `temp.parse().unwrap()`, so a malformed `t=` line panics despite the `io::Result` return type.
  - CRC "validation" only checks the kernel's `crc=YES` string; no independent CRC-8 recompute.
  - README example imports a nonexistent `read_from_device` (real fn is `read_from_probe`) and passes a device ID to `read_from_file`.
  - No discovery, no bulk read, no resolution/conv_time, no 85 °C power-on reset detection, sysfs root hardcoded.
  - Built on the nom 5 parser API.
- **Source 26 (NEW):** Raspberry Pi Forums, "Temperature reading code not working?" [https://forums.raspberrypi.com/viewtopic.php?t=354551](https://forums.raspberrypi.com/viewtopic.php?t=354551) Informal. Poster notes w1thermsensor reads take just under a second each, sequentially; knows of no Python module implementing kernel bulk read; suggests asyncio/threading as a workaround. Useful as motivation and as evidence the concurrency baseline is a real user pattern.

### Claim 6: Kernel bulk conversion exists and is used by some tools, but carries a stale-read hazard that existing consumers don't prevent at the API level

*(Primary kernel docs + ecosystem evidence.)*

- **Source 27:** Linux kernel docs, "Kernel driver w1_therm". [https://docs.kernel.org/w1/slaves/w1_therm.html](https://docs.kernel.org/w1/slaves/w1_therm.html) Writing `trigger` to `therm_bulk_read` at the bus-master level sends Convert T to all devices; reading it returns 0 / -1 / 1 status.
- **Source 28:** Linux kernel sysfs ABI doc, `sysfs-driver-w1_therm`. [https://www.kernel.org/doc/Documentation/ABI/testing/sysfs-driver-w1_therm](https://www.kernel.org/doc/Documentation/ABI/testing/sysfs-driver-w1_therm) States that if a sensor isn't read immediately after a bulk trigger, its next read returns the value from bulk-trigger time, not the current temperature. **This is the core motivation for the stale-read-safe API.**
- **Source 29:** w1-therm-api (PyPI). [https://pypi.org/project/w1-therm-api/](https://pypi.org/project/w1-therm-api/) Python prior art that **does** implement bulk read. Findings from reading `api.py` (repo: https://github.com/troxel/w1-therm-api):
  - Confirmed genuine bulk read: `start_conversion()` writes `trigger` to each master's `therm_bulk_read`, `wait_for_conversion()` polls every 5 ms until each master reads `1`, then `read_temperatures()` loops over sensors reading each cached `temperature` file. The per-sensor loop is expected, since each sensor has its own sysfs file.
  - No freshness enforcement: `start_conversion()` and `read_temperatures()` are separate public calls. Calling `read_temperatures()` without `convert()` silently does sequential per-sensor conversions; reading long after a trigger silently returns trigger-time values; partial reads followed by `read_temperatures()` silently mix stale bulk values and fresh per-sensor conversions.
  - All failures (missing sensor, CRC failure, unreadable file, power-on value) return `None`; trigger failures are only printed as warnings.
  - `read_temperature()` falls back to `w1_slave` if `temperature` is invalid or `85000`. Per the kernel ABI, that second access likely starts a fresh ~750 ms conversion, so one bad read can silently add a full conversion time.
  - Treats every `85000` as the power-on reset value, discarding genuine 85 °C readings.
  - Annotations like `tuple[str, float | None]` without `from __future__ import annotations` mean it requires Python 3.10+, despite its README claiming 3.7+.
  - Small and new: 3 commits, 0 stars. Released on PyPI.
- **Source 30:** wirenboard/wb-mqtt-w1. [https://github.com/wirenboard/wb-mqtt-w1](https://github.com/wirenboard/wb-mqtt-w1) C++ daemon prior art. Detects `therm_bulk_read` per bus master and runs a bulk read when supported. Application, not a reusable library.
- **Source 31 (NEW):** w1thermsensor PR #120, "Add bulk read temperature method" (SimonInOps, opened Feb 13, 2025). [https://github.com/timofurrer/w1thermsensor/pull/120](https://github.com/timofurrer/w1thermsensor/pull/120) Status: open, unmerged, awaiting maintainer review (maintainer noted he has no sensors to test with). Solves issue #115, where multiple users requested bulk read. Author reports 5 DS18B20s at 12-bit going from ~4.02 s (`get_temperature()` loop) to ~0.90 s (bulk read); a user confirmed in Feb 2026 that 7 sensors dropped from ~4.8 s to ~0.8 s. Informal (unreviewed PR comments): cite as a sanity check for expected effect size and as evidence of user demand, not as a measured result.

### Supporting/context sources (not tied to one claim)

- **Source 22:** Sharma, Sharma, Tanksalkar, Torres-Arias & Machiry (Purdue), "Rust for Embedded Systems: Current State and Open Problems," ACM CCS '24. [https://doi.org/10.1145/3658644.3690275](https://doi.org/10.1145/3658644.3690275) Peer-reviewed, top-tier venue. General framing for Rust's value/gaps in embedded systems.
- **Source 23:** "Overview of Embedded Rust Operating Systems and Frameworks" (2024). [https://www.ncbi.nlm.nih.gov/pmc/articles/PMC11398098/](https://www.ncbi.nlm.nih.gov/pmc/articles/PMC11398098/) Peer-reviewed (Sensors/PMC). General Rust-for-sensor-nodes framing.
- **Source 24:** Adafruit, "Comparing libgpiod and gpiozero speeds on the Raspberry Pi 5". [https://blog.adafruit.com/2023/10/26/comparing-libgpiod-and-gpiozero-speeds-on-the-raspberry-pi-5/](https://blog.adafruit.com/2023/10/26/comparing-libgpiod-and-gpiozero-speeds-on-the-raspberry-pi-5/) Informal blog. Logic-analyzer-measured GPIO library comparison on Pi 5.

## Working Notes

### Core project

- Minimum scope: Rust crate for the Linux kernel's 1-Wire (w1-gpio/w1-therm) sysfs interface at `/sys/bus/w1/devices/`: device discovery, `w1_slave` parsing, CRC validation, individual reads, and stale-safe bulk conversion with typed errors.
- Beyond minimum: sensor configuration (resolution, conv_time).
- Fills a real gap: existing Rust 1-Wire crates (one-wire-hal, ds18b20) bit-bang for bare-metal/embedded-hal use. The one Rust sysfs consumer (w1_therm_reader) is a single-sensor parser with no discovery, bulk read, or configuration.

### Python bindings

- Stretch goal per `docs/project-definition.md`, not part of the minimum scope.
- Wrap via PyO3, package with maturin.
- Must release the GIL (`py.allow_threads`) during blocking file reads. A real technical point to write up, not just plumbing.

### Benchmark plan

- Primary baselines (required): w1thermsensor 2.3.0 released (sequential) and w1-therm-api (bulk). Released packages are preferred because they are what users install and reviewers can reproduce them from PyPI.
- Optional baselines (only if time allows, since benchmark time is the main feasibility risk): w1thermsensor `AsyncW1ThermSensor` + `asyncio.gather`; w1thermsensor PR #120 branch (record the commit hash). If PR #120 isn't run, cite its reported numbers instead.
- This crate native Rust is required; this crate via Python bindings only if the stretch goal is reached.
- Regime 1: single sensor, normal polling. Expect the \~750ms conversion wait to dominate; Rust-vs-Python difference likely noise.
- Regime 2: many sensors. Expect per-sensor overhead to compound and become visible. Sensor counts: fixed levels within 1 to 10 sensors on one bus (e.g. 1, 2, 5, 10), finalized before benchmarking.
- Regime 3 (optional): non-thermal 1-Wire device (e.g. DS2413) with no conversion delay, isolating pure per-call overhead.
- Repeat on at least two Linux distributions on the same Pi 5, Raspberry Pi OS first.
- Let regimes 2/3 carry the speed claim; don't oversell regime 1.
- Comparison logic for Regime 2:
  - w1thermsensor sequential vs. w1-therm-api bulk → isolates the **architecture** effect (crosses two codebases, but both are thin Python sysfs readers, so the confound is small).
  - w1-therm-api bulk vs. this crate bulk → isolates the **language/API** effect.
  - w1thermsensor async vs. bulk (if run) → tests whether concurrency alone closes the gap.
- Performance targets: single sensor, parity with both Python libraries (conversion time dominates); multiple sensors, clearly faster than w1thermsensor sequential and at least competitive with w1-therm-api bulk.

### Harness rules

- w1-therm-api bulk baseline must call `convert()` then `read_temperatures()`. `read_temperatures()` alone benchmarks the sequential path and invalidates the comparison.
- Use `unit="C"` for w1-therm-api so outputs match the other libraries.
- Time the trigger, wait, and readout phases separately where the library allows it.
- Log which read path was used and count None/error results, so fallback-induced outliers can be explained.
- CPU usage includes w1-therm-api's 5 ms polling loop; note this when comparing CPU numbers, and document how this crate waits.
- Record kernel version, OS/distribution version, Python version, library versions, sensor count, and resolution with every result.

### Advantage over existing bit-banging Rust crates

- Those crates assume bare-metal, no-OS timing (nothing preempts mid-pulse).
- Doing that same bit-banging in Linux userspace means fighting the OS scheduler. Same jitter problem seen elsewhere on Pi (RP1 GPIO jitter, WS2812 moving off bit-banged GPIO, isolcpus/taskset workarounds).
- This crate sidesteps that by not bit-banging at all. It consumes the kernel's already-produced result.
- Honesty caveat: the kernel's own w1-gpio driver is *also* software bit-banging (in kernel space) and has documented issues at scale plus a known Pi 5/RP1 instability bug. The crate inherits whatever the kernel gets wrong. Claim is "avoids reimplementing the timing problem myself," not "immune to it."

### Verified: real feature gap, not just a language gap

- Confirmed by reading w1thermsensor's source directly (multiple released versions): each sensor read is independent, blocking file I/O; `therm_bulk_read` is never used in a released version, so multi-sensor reads pay the full conversion delay serially, per sensor. Cite the specific version tags checked.
- Confirmed from w1thermsensor README (PyPI 2.3.0): the documented multi-sensor pattern is a sequential loop. Note the README alone only shows absence from the docs; cite the source check as the real evidence.
- CRC checking is implemented correctly in w1thermsensor. The gap is specifically the missing bulk-trigger optimization, not general carelessness.
- **Correction (9/23):** w1thermsensor *does* consume sysfs `w1_slave`. Sysfs consumption is not the contribution; bulk conversion + stale-read safety + Rust + bindings is.
- **Correction (9/23):** Bulk read is not unique to this project. w1-therm-api (Python) and wb-mqtt-w1 (C++) both use it. Don't claim "first to use bulk read."
- **Correction (9/24):** w1thermsensor bulk read exists as unmerged PR #120 (https://github.com/timofurrer/w1thermsensor/pull/120); claims must be scoped to released versions and must cite the PR.
- **Correction (9/24):** w1-therm-api source reviewed: genuine bulk read, but no freshness enforcement and no typed errors (see Source 29).

### Correctness evaluation

- Unit tests against fixture sysfs directories covering malformed driver output, CRC failures, missing sensors, and power-on reset values.
- Error rate recorded during benchmark runs.
- Stale-read tests showing that reading after a conversion has expired, or reading a sensor twice from one bulk conversion, is rejected by the API rather than returning an old value.

### Open risk: concurrency may shrink the bulk-read advantage

- w1thermsensor's async interface (or threads) could overlap per-sensor conversion waits, recovering part of the sequential-vs-bulk gap.
- Whether it does depends on whether `w1_therm` holds the bus mutex during the conversion sleep. Check `convert_t()` in `drivers/w1/slaves/w1_therm.c` for the target kernel.
- Either result is publishable: if bulk still wins, the claim is stronger; if not, the writeup says so before a reviewer does.

## To-Do List

### Research

- [x] Read the w1_therm.rst kernel doc (bulk read section)
- [ ] Finish the rest of w1_therm.rst end to end (conv_time, features, resolution, alarms) before designing the API
- [x] Read w1thermsensor PyPI docs; confirmed no bulk read, sequential multi-sensor pattern
- [ ] Record the w1thermsensor source check as citable evidence (grep for `therm_bulk_read` across tagged releases, note versions checked)
- [x] Check w1-therm-api in detail: read its source, confirm what it covers, and specifically check whether it guards against stale bulk reads (source reviewed 9/24; findings in Source 29)
- [ ] Re-check crates.io/lib.rs one more time before writing code; confirm whether w1_therm_reader appears in the `onewire` keyword listing (Source 18)
- [ ] Check `convert_t()` locking in `drivers/w1/slaves/w1_therm.c` on the target kernel: is the bus mutex held during the conversion sleep?
- [ ] Confirm the current status of the Pi 5/RP1 w1-gpio instability bug on the target kernel version: still present, or already patched?
- [ ] Verify the exact wording of the \~1µs figure in the RP1 datasheet (Source 15) before quoting it anywhere
- [ ] Review the PR #120 diff (https://github.com/timofurrer/w1thermsensor/pull/120/files) for stale-read handling, therm_bulk_read status checks, missing-sensor handling, and CRC on the bulk path (only needed if running it as a baseline or citing its design)
- [ ] Before final writeup, recheck whether PR #120 has been merged or released and update claim wording if so
- [ ] Confirm from the kernel ABI/source that a second access after a consumed bulk result starts a fresh conversion (supports the w1-therm-api fallback finding)
- [ ] Pick the second Linux distribution for benchmarking and confirm w1-gpio/w1-therm support on Pi 5
- **Priority literature to read before starting development**:
  - [ ] Source 5 (PyO3/Rust bindings overhead study): centerpiece for the speedup claim
  - [ ] Sources 8 & 9 (PREEMPT_RT / userspace-vs-kernel GPIO latency studies): backbone of the bit-banging reliability argument
  - [ ] Source 14 (PIOLib blog): the RP1 latency figure to quote
  - [ ] Sources 18, 19, 21, 25 (crates.io listing, fuchsnj crates, w1thermsensor, w1_therm_reader): the ecosystem-gap evidence itself
  - [ ] Sources 27, 28, 29 (kernel docs, ABI doc, w1-therm-api): the bulk-read and stale-read evidence
  - [ ] Everything else in the claims list above, as time allows, roughly in the order the claims appear in the writeup

### Hardware & Setup

- [ ] Once wired, use the multimeter to measure actual DATA-to-VCC resistance on the DS18B20 bus (up to 10 sensors). If it's far below 4.7kΩ from parallel adapter-board resistors, disable all but one onboard pull-up resistor (route the other sensors' power/data via alligator clips, bypassing their onboard resistor path)
- [ ] Wire the DS2413 on its own separate GPIO pin/w1-gpio overlay instance, with its own single 4.7kΩ resistor, kept isolated from the DS18B20 bus
- [ ] Skip installing headers on the DS2413 board; use the alligator clip leads directly on its pads instead

### Design

- [ ] Sketch public API: discovery, single-read, and a distinct bulk-read type that can't yield stale data by construction
- [ ] Decide error taxonomy: kernel module not loaded, device not found, CRC mismatch, conversion in progress, stale bulk-read access, parse failure (never panic on malformed input), 85 °C power-on reset value
- [ ] Decide CRC approach: recompute CRC-8 from the 9 raw scratchpad bytes, or only check the kernel's `crc=YES/NO`. Describe the feature accurately either way
- [ ] Decide how to handle 85000 (85 °C): treating it as the power-on value discards genuine readings; choose deliberately and document it
- [ ] Design stale-read tests alongside the bulk-read API (expired conversion, double read from one conversion)
- [ ] Decide PyO3 boundary and where `py.allow_threads` is required

### Benchmarking methodology *(define before writing the crate)*

- [ ] Regime 1: single sensor, normal polling
- [ ] Regime 2: many sensors; required baselines = w1thermsensor 2.3.0 sequential, w1-therm-api bulk, this crate (Rust); optional baselines = w1thermsensor async/concurrent, w1thermsensor PR #120 branch, this crate (Python bindings, if built)
- [ ] Regime 3 (optional): non-thermal device, no conversion delay
- [ ] Metrics: wall-clock time, CPU usage, reliability/error-rate count
- [ ] Fix sensor count levels in advance within 1 to 10 (e.g. 1, 2, 5, 10) so compounding is visible on a chart
- [ ] Repeat on a second Linux distribution on the same Pi 5

### Development milestones

- [ ] Rust crate: discovery + single-sensor read with CRC validation
- [ ] Bulk-trigger (`therm_bulk_read`) coordination with stale-read protection
- [ ] conv_time/resolution configuration support (beyond minimum scope)
- [ ] PyO3 bindings + maturin packaging, GIL release verified (stretch goal)
- [ ] Benchmark harness vs. w1thermsensor 2.3.0 and w1-therm-api across all regimes (optional baselines if time allows)
- [ ] Write up results, explicit about where speed matters and where it doesn't

### Writeup / documentation

- [ ] Related-work section built from the claims/sources list above, tiered honestly (peer-reviewed vs. arXiv preprint vs. primary docs vs. informal)
- [ ] Related-work must acknowledge w1_therm_reader, w1-therm-api, wb-mqtt-w1, and w1thermsensor PR #120 as prior art, and state precisely what this project adds
- [ ] Explicitly document the caveats: kernel bit-bang isn't immune to jitter either; RP1 latency figures are primary-source, not academic; PyO3 doesn't eliminate FFI overhead; concurrency may narrow the bulk-read advantage
- [ ] Note open questions/risks: PCIe-generation inconsistency in Raspberry Pi's own docs (Gen 2 x4 vs Gen 3 claims across their posts), whether the target kernel still has the RP1 w1-gpio bug

## Changelog

- **9/16/2026:** Initial notes, claims 1–5, to-do list.
- **9/23/2026:** Revised Claim 5 (w1_therm_reader found; "none consume sysfs" was false). Added Claim 6 and Sources 25–30. Corrected w1thermsensor description (consumes sysfs, no bulk read, has async interface). Added contribution statement, concurrency risk, w1-therm-api and async baselines to Regime 2, new research/design to-dos. Marked kernel bulk-read doc and w1thermsensor PyPI review as done.
- **9/24/2026:** Adopted docs/project-definition.md as authoritative. Renamed to rs1wire. Scoped w1thermsensor claim to released versions and added Source 31 (PR #120). Recorded w1-therm-api source review in Source 29. Restructured benchmark plan into required (w1thermsensor 2.3.0, w1-therm-api) and optional (async, PR #120) baselines, sensor counts 1 to 10, two distributions, and harness rules. Marked configuration as beyond minimum scope and Python bindings as stretch. Added correctness evaluation, 85 °C decision, and new research/design to-dos.