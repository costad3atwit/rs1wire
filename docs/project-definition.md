# rs1wire Project Definition

This document is the authoritative definition of the project. Supporting research, sources, and working notes live in `docs/planning.md`.

## 1. Project Area

Embedded systems / systems software development

## 2. Problem

Developers reading 1-Wire temperature sensors on Linux single-board computers such as the Raspberry Pi have no maintained Rust library that uses the kernel's w1 driver. Existing Rust 1-Wire crates are designed for bare-metal microcontrollers and implement the protocol's timing in software, which is vulnerable to scheduler jitter when run from Linux userspace. The one Rust crate that reads the kernel driver's output (w1_therm_reader) is an unmaintained single-sensor parser without discovery, bulk reading, or configuration. In Python, a widely used library (w1thermsensor, packaged in Raspberry Pi OS) reads sensors one at a time in its released versions, so reading several sensors costs roughly one full conversion time (~750 ms) per sensor. The kernel offers a bulk conversion that reads every sensor on a bus in about one conversion time, but it can return stale values if a sensor is read too long after the conversion is triggered, and no existing library prevents that at the API level.

## 3. Proposed Artifact/Approach

I will build a Rust crate that discovers, configures, and reads 1-Wire temperature sensors through the Linux kernel's w1 driver, both individually and in bulk. The bulk-read API will be designed so stale values cannot be read by mistake, and errors will be reported with distinct types rather than a generic failure value. As a stretch goal, I will provide Python bindings using PyO3. I will benchmark the crate against the released Python libraries w1thermsensor (sequential reads) and w1-therm-api (bulk reads).

## 4. Objectives

a. A Rust crate that discovers DS18B20 sensors, reads them individually and in bulk through the kernel's `therm_bulk_read` interface, configures sensor resolution, and prevents stale bulk reads by construction.

b. A reproducible benchmark on a Raspberry Pi 5 comparing the crate against w1thermsensor and w1-therm-api at fixed sensor counts (1 to 10), measuring wall-clock time, CPU usage, and error rate, and repeated on at least two Linux distributions running on the same board.

c. Stretch goal: Python bindings so the crate can be used from Python as an alternative to the existing libraries.

## 5. Evidence/Evaluation

Correctness will be shown through unit tests run against sample sysfs directories, including malformed driver output, CRC failures, missing sensors, and power-on reset values, plus the error rate recorded during benchmark runs.

Performance will be evaluated per scenario. With a single sensor, the ~750 ms conversion time dominates, so the target is parity with both Python libraries. With multiple sensors, the target is being clearly faster than w1thermsensor's released sequential reads and at least competitive with w1-therm-api's bulk reads.

Stale-read prevention will be demonstrated with tests showing that reading after a conversion has expired, or reading a sensor twice from one bulk conversion, is rejected by the API rather than returning an old value.

Independent measurements reported in an open w1thermsensor pull request (about 4.0 s down to 0.9 s for 5 sensors using bulk read) provide a sanity check for the expected size of the multi-sensor improvement.

## 6. Minimum Scope

The smallest meaningful version is a crate that discovers sensors, reads them individually, and performs stale-safe bulk reads with typed errors, tested on Raspberry Pi OS and benchmarked against both Python libraries. Configuration support and Python bindings are extensions beyond the minimum.

## 7. Non-Goals

a. I will not attempt to support all 1-Wire device types. Temperature sensing is a major use of 1-Wire, but the protocol is also used for power supply identification, access control fobs, and storing calibration data. The Linux kernel supports these devices, but including them is not feasible within this project. A DS2413 switch may be used only to measure per-call overhead in benchmarks, not as a supported device. Broader device support may be future work.

b. I will not test or benchmark on hardware other than a Raspberry Pi 5 with DS18B20 sensors (and the DS2413 noted above), since that is the hardware already purchased and there is no additional budget. Benchmarks will therefore cover up to 10 sensors on one bus.

c. I will not implement 1-Wire timing in userspace. The crate relies entirely on the kernel driver, which avoids reintroducing the timing and scheduler-jitter problems that userspace implementations face.

## 8. Major Feasibility Concern

The biggest risk is hardware setup and baseline benchmarking taking longer than planned. If this takes more than two weeks, it will significantly delay development. A known instability in the w1-gpio driver on the Raspberry Pi 5 may make readings unreliable on some kernel versions and could cause this delay. I will check its status early and pin the kernel version where it is resolved.