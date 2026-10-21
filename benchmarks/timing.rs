//! rs1wire benchmark: sequential and bulk reads through the crate, appended to
//! the same `timings.csv` as `timing.py`, with the same columns.
//!
//! Every trial runs each method at each sensor count, reading the first N
//! sensors (sorted by ROM ID, the same order `timing.py` uses), and appends one
//! row per (method, N). Must run on the Raspberry Pi, from a release build:
//!
//! ```text
//! cargo build --release --example timing
//! ./target/release/examples/timing                  # 30 trials at counts 1, 2, 5, 10
//! ./target/release/examples/timing --trials 3       # quick smoke test
//! ./target/release/examples/timing --methods rs1wire-bulk --counts 1 5 10
//! ./target/release/examples/timing --seed 1234 --gap 2
//! ```
//!
//! Methods:
//!
//! ```text
//! rs1wire-seq    Sensor::read() on each sensor in turn (w1_slave)
//! rs1wire-bulk   BusMaster::bulk_convert(), then BulkConversion::read() on
//!                each of the N sensors (w1_slave)
//! ```
//!
//! Phases for bulk: `trigger_s` is the `bulk_convert()` call, which includes
//! the whole conversion because the kernel's `trigger` write is synchronous.
//! There is no separate wait phase, so `wait_s` is left empty. `readout_s`
//! covers the N reads.
//!
//! A bulk trigger converts every sensor on the bus. The conversion is dropped
//! after the timed region, which drains the sensors that weren't read, so the
//! drain is untimed, as in `timing.py`. `therm_bulk_read` is checked before
//! every cell and after each bulk cell; a pending result is recorded as
//! `BulkPendingBeforeRun` or `BulkPendingAfterDrain` in `error_types`.
//!
//! Bus search is disabled while timing when the full bus is present, and
//! restored on exit (including Ctrl-C), exactly as in `timing.py`.
//!
//! The cell shuffle uses its own PRNG, so a seed gives a reproducible order
//! here but not the same order as the same seed in `timing.py`. The two
//! harnesses run as separate processes, so cells are not interleaved across
//! them.

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rs1wire::{BulkOptions, BulkStatus, BusMaster, Error, Sensor, W1};

/// Same columns, same order as `FIELDS` in `timing.py`.
const FIELDS: [&str; 33] = [
    "run_id", "timestamp_utc", "library", "library_version", "library_source",
    "method", "read_path", "sensor_count", "bus_sensor_count", "trial",
    "run_order", "seed", "gap_s",
    "wall_s", "trigger_s", "wait_s", "readout_s",
    "cpu_total_s", "cpu_user_s", "cpu_sys_s",
    "reads_ok", "reads_failed", "error_types",
    "resolution_bits", "conv_time_ms", "w1_master_search",
    "kernel", "os", "os_id", "os_version", "python", "rustc", "hostname",
];

const DEFAULT_COUNTS: [usize; 4] = [1, 2, 5, 10];
/// Search is disabled only when the full bus is present.
const EXPECTED_SENSORS: usize = 10;

/// label, method, read_path. Output rows are sorted in this order.
const METHODS: [(&str, &str, &str); 2] = [
    ("rs1wire-seq", "sequential", "w1_slave"),
    ("rs1wire-bulk", "bulk", "w1_slave"),
];

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

type Row = Vec<(&'static str, String)>;

fn run() -> Result<(), String> {
    let args = Args::parse()?;
    if cfg!(debug_assertions) && !args.allow_debug {
        return Err("Debug build: timings would not be representative. Build with \
                    `cargo build --release --example timing`, or pass --allow-debug."
            .into());
    }
    // Set a flag instead of dying, so search is restored and only complete
    // trials are written. Checked between cells.
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is signal safe.
        unsafe {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }

    let seed = args.seed.unwrap_or_else(random_seed);
    let mut rng = SplitMix64(u64::from(seed));

    // Discovery happens once, outside the timed region.
    let w1 = W1::new();
    let mut buses: Vec<(BusMaster, Vec<Sensor>)> = Vec::new();
    for bus in w1.bus_masters().map_err(|e| e.to_string())? {
        let sensors = bus.sensors().map_err(|e| e.to_string())?;
        if !sensors.is_empty() {
            buses.push((bus, sensors));
        }
    }
    let max_count = args.counts.iter().copied().max().unwrap_or(0);
    let found: usize = buses.iter().map(|(_, s)| s.len()).sum();
    if found < max_count {
        return Err(format!(
            "Found {found} sensors but need {max_count}. Check wiring and /sys/bus/w1/devices/."
        ));
    }
    if buses.len() != 1 {
        return Err(format!(
            "Expected every DS18B20 on one bus master, found them on {}.",
            buses.len()
        ));
    }
    let (mut bus, sensors) = buses.remove(0);

    let machine = common_env();
    let lib = library_meta();
    let run_id = utc_now("%Y%m%dT%H%M%SZ");
    let bus_count = read_attr(&bus.path().join("w1_master_slave_count"));
    let per_count: Vec<(usize, Row)> = args
        .counts
        .iter()
        .map(|&n| {
            let first = &sensors[..n];
            (n, vec![
                ("resolution_bits", joined(first, |s| s.resolution().map(|r| r.bits().to_string()))),
                ("conv_time_ms", joined(first, |s| s.conv_time().map(|d| d.as_millis().to_string()))),
                ("bus_sensor_count", bus_count.clone()),
            ])
        })
        .collect();

    println!(
        "run {run_id}: kernel {}, {} ({} {})",
        machine[0].1, machine[1].1, machine[2].1, machine[3].1
    );
    println!("  rs1wire {} ({}), {}", lib[1].1, lib[2].1, machine[5].1);
    println!("methods: {}", args.methods.join(", "));
    println!("seed {seed}, gap {:?}s", args.gap);
    let ids: Vec<String> = sensors[..max_count].iter().map(|s| s.id().to_string()).collect();
    println!("sensors (sorted): {}", ids.join(", "));

    // Open the CSV before touching the bus, so a schema mismatch exits
    // without leaving search disabled.
    let csv_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/timings.csv");
    let mut csv = open_csv(&csv_path)?;

    // Restored on drop, which also covers an early return or a panic.
    let _search = if sensors.len() >= EXPECTED_SENSORS {
        let guard = SearchGuard::disable(bus.path())?;
        println!("bus search disabled on {} (was {})", bus.name(), guard.original);
        Some(guard)
    } else {
        println!("{}/{EXPECTED_SENSORS} sensors found: leaving kernel bus search on", sensors.len());
        None
    };
    let search = read_attr(&bus.path().join("w1_master_search"));

    let warmup = i64::from(args.warmup);
    'trials: for trial in -warmup..i64::from(args.trials) {
        let mut cells: Vec<(usize, usize)> = (0..METHODS.len())
            .filter(|&m| args.methods.contains(&METHODS[m].0))
            .flat_map(|m| args.counts.iter().map(move |&n| (m, n)))
            .collect();
        rng.shuffle(&mut cells);

        let mut rows: Vec<(usize, usize, Row)> = Vec::new();
        for (run_order, &(m, n)) in cells.iter().enumerate() {
            let (label, method, read_path) = METHODS[m];
            let result = run_cell(&mut bus, &sensors[..n], label, n);
            if STOP.load(Ordering::SeqCst) {
                println!("interrupted: discarding the incomplete trial");
                break 'trials;
            }
            let mut row: Row = vec![
                ("run_id", run_id.clone()),
                ("timestamp_utc", utc_now("%Y-%m-%dT%H:%M:%S+00:00")),
                ("method", method.into()),
                ("read_path", read_path.into()),
                ("sensor_count", n.to_string()),
                ("trial", trial.to_string()),
                ("run_order", run_order.to_string()),
                ("seed", seed.to_string()),
                ("gap_s", format!("{:?}", args.gap)),
                ("w1_master_search", search.clone()),
            ];
            row.extend(per_count.iter().find(|(c, _)| *c == n).map(|(_, r)| r.clone()).unwrap_or_default());
            row.extend(lib.iter().cloned());
            row.extend(machine.iter().cloned());
            row.extend(result);
            rows.push((m, n, row));
            if args.gap > 0.0 {
                thread::sleep(Duration::from_secs_f64(args.gap));
            }
        }
        if trial >= 0 {
            // Written grouped by method (METHODS order), then N.
            rows.sort_by_key(|&(m, n, _)| (m, n));
            for (_, _, row) in &rows {
                write_row(&mut csv, row).map_err(|e| format!("writing {}: {e}", csv_path.display()))?;
            }
            csv.flush().map_err(|e| e.to_string())?;
        }
        if trial < 0 {
            println!("  warmup done");
        } else {
            println!("  trial {}/{} done", trial + 1, args.trials);
        }
    }

    println!("Results appended to {}", csv_path.display());
    Ok(())
}

/// Runs one (method, N) cell. Pending bulk results are drained before and
/// after, outside the timed region.
fn run_cell(bus: &mut BusMaster, sensors: &[Sensor], label: &str, n: usize) -> Row {
    let mut flags = Vec::new();
    if bulk_pending(bus) {
        let _ = bus.discard_pending();
        flags.push("BulkPendingBeforeRun".to_owned());
        eprintln!("WARNING: bulk result pending before {label} N={n}; drained");
    }

    let mut ok = 0;
    let mut errors: Vec<String> = Vec::new();
    let mut phases: Row = Vec::new();
    let meter = Meter::start();
    let usage;
    if label == "rs1wire-seq" {
        for sensor in sensors {
            match sensor.read() {
                Ok(_) => ok += 1,
                Err(e) => errors.push(error_name(&e)),
            }
        }
        usage = meter.stop();
    } else {
        let t0 = Instant::now();
        // Matched directly (not bound first), so the conversion's borrow of
        // `bus` ends with the match and the drain check below can use it.
        match bus.bulk_convert(BulkOptions::default()) {
            Ok(mut conversion) => {
                let t1 = Instant::now();
                phases.push(("trigger_s", secs(t1 - t0)));
                for sensor in sensors {
                    match conversion.read(&sensor.id()) {
                        Ok(_) => ok += 1,
                        Err(e) => errors.push(error_name(&e)),
                    }
                }
                phases.push(("readout_s", secs(t1.elapsed())));
                usage = meter.stop();
                // Untimed: dropping drains the sensors that weren't read.
                drop(conversion);
            }
            Err(e) => {
                phases.push(("trigger_s", secs(t0.elapsed())));
                usage = meter.stop();
                errors.extend(std::iter::repeat_n(error_name(&e), n));
            }
        }
        if bulk_pending(bus) {
            let _ = bus.discard_pending();
            flags.push("BulkPendingAfterDrain".to_owned());
            eprintln!("WARNING: bulk result still pending after {label} N={n}");
        }
    }

    let types: BTreeSet<String> = errors.iter().cloned().chain(flags).collect();
    let mut row: Row = vec![
        ("wall_s", secs(usage.wall)),
        ("cpu_total_s", secs(usage.cpu)),
        ("cpu_user_s", secs(usage.user)),
        ("cpu_sys_s", secs(usage.sys)),
        ("reads_ok", ok.to_string()),
        ("reads_failed", errors.len().to_string()),
        ("error_types", types.into_iter().collect::<Vec<_>>().join(";")),
    ];
    row.extend(phases);
    row
}

fn bulk_pending(bus: &BusMaster) -> bool {
    matches!(bus.bulk_status(), Ok(BulkStatus::InProgress | BulkStatus::CompleteUnread))
}

/// The variant name, plus the I/O error kind for `Io`, so `error_types`
/// separates e.g. permission failures from kernel errors.
fn error_name(e: &Error) -> String {
    let name = match e {
        Error::ModuleNotLoaded { .. } => "ModuleNotLoaded",
        Error::DeviceNotFound { .. } => "DeviceNotFound",
        Error::BusNotFound { .. } => "BusNotFound",
        Error::BulkReadUnsupported { .. } => "BulkReadUnsupported",
        Error::CrcMismatch { .. } => "CrcMismatch",
        Error::ConversionInProgress { .. } => "ConversionInProgress",
        Error::StaleBulkRead { .. } => "StaleBulkRead",
        Error::ReadFailed { .. } => "ReadFailed",
        Error::Parse { .. } => "Parse",
        Error::PowerOnReset { .. } => "PowerOnReset",
        Error::Io { source, .. } => return format!("Io({:?})", source.kind()),
        _ => "Other",
    };
    name.to_owned()
}

/// Wall clock and CPU time, like `measure()` in `timing.py`: total CPU from
/// `CLOCK_PROCESS_CPUTIME_ID` (what Python's `process_time()` uses), and the
/// user/sys split from `getrusage`, which is tick-sampled and approximate.
struct Meter {
    wall: Instant,
    cpu: Duration,
    rusage: (Duration, Duration),
}

struct Usage {
    wall: Duration,
    cpu: Duration,
    user: Duration,
    sys: Duration,
}

impl Meter {
    fn start() -> Self {
        let rusage = rusage();
        let cpu = process_cpu();
        Self {
            wall: Instant::now(),
            cpu,
            rusage,
        }
    }

    fn stop(&self) -> Usage {
        let wall = self.wall.elapsed();
        let cpu = process_cpu().saturating_sub(self.cpu);
        let (user, sys) = rusage();
        Usage {
            wall,
            cpu,
            user: user.saturating_sub(self.rusage.0),
            sys: sys.saturating_sub(self.rusage.1),
        }
    }
}

fn process_cpu() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return Duration::ZERO;
    }
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

fn rusage() -> (Duration, Duration) {
    // SAFETY: an all-zero rusage is a valid value, and getrusage only writes
    // into the struct it is given.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return (Duration::ZERO, Duration::ZERO);
    }
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    (tv(ru.ru_utime), tv(ru.ru_stime))
}

fn secs(d: Duration) -> String {
    format!("{:.6}", d.as_secs_f64())
}

/// One value per distinct result across sensors, `a;b` if they differ.
fn joined(sensors: &[Sensor], f: impl Fn(&Sensor) -> rs1wire::Result<String>) -> String {
    let values: BTreeSet<String> = sensors
        .iter()
        .map(|s| f(s).unwrap_or_else(|_| "unknown".into()))
        .collect();
    values.into_iter().collect::<Vec<_>>().join(";")
}

// --- Metadata ---

/// kernel, os, os_id, os_version, python, rustc, hostname (in that order,
/// which `run()` relies on for printing).
fn common_env() -> Row {
    let release = os_release();
    let get = |key: &str| release.get(key).cloned().unwrap_or_else(|| "unknown".into());
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let rustc = command_output("rustc", &["--version"])
        .map(|v| format!("{v} ({profile})"))
        .unwrap_or_else(|| format!("unknown ({profile})"));
    vec![
        ("kernel", read_attr(Path::new("/proc/sys/kernel/osrelease"))),
        ("os", get("PRETTY_NAME")),
        ("os_id", get("ID")),
        ("os_version", get("VERSION_ID")),
        // Python rows record the interpreter here; Rust rows leave it empty.
        ("python", String::new()),
        ("rustc", rustc),
        ("hostname", read_attr(Path::new("/proc/sys/kernel/hostname"))),
    ]
}

/// library, library_version, library_source. The source is the git commit
/// the crate was built from, with `-dirty` if the crate's own sources or this
/// harness have uncommitted changes (timings.csv itself doesn't count).
fn library_meta() -> Row {
    let source = match command_output("git", &["rev-parse", "--short=12", "HEAD"]) {
        Some(commit) => {
            let dirty = command_output(
                "git",
                &["status", "--porcelain", "--", "src", "Cargo.toml", "Cargo.lock", "benchmarks/timing.rs"],
            )
            .is_some_and(|s| !s.is_empty());
            format!("git:{commit}{}", if dirty { "-dirty" } else { "" })
        }
        None => "unknown".into(),
    };
    vec![
        ("library", "rs1wire".into()),
        ("library_version", env!("CARGO_PKG_VERSION").into()),
        ("library_source", source),
    ]
}

/// Runs a command in the crate directory, so `rustc` follows
/// rust-toolchain.toml and `git` sees this repository.
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Parses os-release the way Python's `platform.freedesktop_os_release()`
/// does: `/etc/os-release`, else `/usr/lib/os-release`, KEY=VALUE lines with
/// optional quotes.
fn os_release() -> std::collections::HashMap<String, String> {
    let content = fs::read_to_string("/etc/os-release")
        .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    content
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), unquote(value.trim())))
        .collect()
}

fn unquote(value: &str) -> String {
    let inner = ['"', '\'']
        .iter()
        .find_map(|&q| value.strip_prefix(q).and_then(|v| v.strip_suffix(q)))
        .unwrap_or(value);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn read_attr(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "unknown".into())
}

/// The current UTC time. Supports only the `%Y %m %d %H %M %S` fields used here.
fn utc_now(format: &str) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format
        .replace("%Y", &format!("{year:04}"))
        .replace("%m", &format!("{month:02}"))
        .replace("%d", &format!("{day:02}"))
        .replace("%H", &format!("{:02}", rem / 3600))
        .replace("%M", &format!("{:02}", rem % 3600 / 60))
        .replace("%S", &format!("{:02}", rem % 60))
}

/// Days since 1970-01-01 to (year, month, day), from Howard Hinnant's
/// `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn random_seed() -> u32 {
    let mut buf = [0u8; 4];
    if File::open("/dev/urandom")
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut buf))
        .is_ok()
    {
        return u32::from_be_bytes(buf);
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// SplitMix64: small, seedable, good enough to shuffle a few cells.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Fisher–Yates.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

// --- Bus search ---

/// Disables `w1_master_search` and restores the original value on drop.
struct SearchGuard {
    path: PathBuf,
    original: String,
}

impl SearchGuard {
    fn disable(bus: &Path) -> Result<Self, String> {
        let path = bus.join("w1_master_search");
        let original = read_attr(&path);
        write_sysfs(&path, "0").map_err(|e| {
            format!(
                "Could not disable search on {}: {e}\n\
                 Needs passwordless sudo, or run as root.",
                bus.display()
            )
        })?;
        let guard = Self { path, original };
        let now = read_attr(&guard.path);
        if now != "0" {
            return Err(format!("Search not disabled (w1_master_search = {now})."));
        }
        Ok(guard)
    }
}

impl Drop for SearchGuard {
    fn drop(&mut self) {
        if self.original == "unknown" {
            return;
        }
        // Warn rather than fail, so a restore problem can't mask the
        // original error.
        match write_sysfs(&self.path, &self.original) {
            Ok(()) => println!("bus search restored ({})", read_attr(&self.path)),
            Err(e) => eprintln!(
                "WARNING: could not restore search to {}: {e}",
                self.original
            ),
        }
    }
}

/// Writes a root-only sysfs attribute: directly if permitted, else through
/// `sudo -n tee` (non-interactive, fails instead of prompting).
fn write_sysfs(path: &Path, value: &str) -> Result<(), String> {
    match OpenOptions::new().write(true).truncate(true).open(path) {
        Ok(mut f) => return f.write_all(value.as_bytes()).map_err(|e| e.to_string()),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
        Err(e) => return Err(e.to_string()),
    }
    let mut child = Command::new("sudo")
        .args(["-n", "tee"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sudo: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(value.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if stderr.is_empty() {
            format!("sudo exited {}", out.status)
        } else {
            stderr
        })
    }
}

// --- CSV ---

/// Opens for appending; refuses to mix schemas with an existing file.
fn open_csv(path: &Path) -> Result<File, String> {
    let is_new = fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true);
    if !is_new {
        let mut header = String::new();
        File::open(path)
            .and_then(|f| BufReader::new(f).read_line(&mut header))
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        if header.trim_end_matches(['\r', '\n']) != FIELDS.join(",") {
            return Err(format!(
                "{} exists with a different header. Rename or empty it, then rerun.",
                path.display()
            ));
        }
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    if is_new {
        write_line(&mut f, FIELDS.iter().map(|s| s.to_string()))
            .map_err(|e| e.to_string())?;
    }
    Ok(f)
}

/// Writes one row in `FIELDS` order; missing columns are empty, like
/// Python's `DictWriter`.
fn write_row(f: &mut File, row: &Row) -> io::Result<()> {
    write_line(
        f,
        FIELDS.iter().map(|field| {
            // Later entries win, matching the `**result` override in timing.py.
            row.iter()
                .rev()
                .find(|(k, _)| k == field)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        }),
    )
}

/// One CSV record with Python's `csv` defaults: quote only when needed, and
/// `\r\n` line endings.
fn write_line(f: &mut File, values: impl Iterator<Item = String>) -> io::Result<()> {
    let mut line = String::new();
    for (i, value) in values.enumerate() {
        if i > 0 {
            line.push(',');
        }
        if value.contains([',', '"', '\r', '\n']) {
            let _ = write!(line, "\"{}\"", value.replace('"', "\"\""));
        } else {
            line.push_str(&value);
        }
    }
    line.push_str("\r\n");
    f.write_all(line.as_bytes())
}

// --- Arguments ---

struct Args {
    trials: u32,
    warmup: u32,
    counts: Vec<usize>,
    methods: Vec<&'static str>,
    seed: Option<u32>,
    gap: f64,
    allow_debug: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Self {
            trials: 30,
            warmup: 1,
            counts: DEFAULT_COUNTS.to_vec(),
            methods: METHODS.iter().map(|m| m.0).collect(),
            seed: None,
            gap: 0.0,
            allow_debug: false,
        };
        let raw: Vec<String> = env::args().skip(1).collect();
        let mut i = 0;
        // Values for a flag: everything up to the next `--flag`.
        let values = |i: &mut usize| -> Vec<String> {
            let start = *i + 1;
            let mut end = start;
            while end < raw.len() && !raw[end].starts_with("--") {
                end += 1;
            }
            *i = end;
            raw[start..end].to_vec()
        };
        fn one<T: std::str::FromStr>(flag: &str, v: Vec<String>) -> Result<T, String> {
            match v.as_slice() {
                [x] => x.parse().map_err(|_| format!("{flag}: invalid value {x:?}")),
                _ => Err(format!("{flag} takes exactly one value")),
            }
        }
        while i < raw.len() {
            let flag = raw[i].clone();
            match flag.as_str() {
                "--trials" => args.trials = one(&flag, values(&mut i))?,
                "--warmup" => args.warmup = one(&flag, values(&mut i))?,
                "--seed" => args.seed = Some(one(&flag, values(&mut i))?),
                "--gap" => args.gap = one(&flag, values(&mut i))?,
                "--counts" => {
                    let v = values(&mut i);
                    let mut counts = v
                        .iter()
                        .map(|x| x.parse::<usize>().ok().filter(|&n| n > 0))
                        .collect::<Option<Vec<_>>>()
                        .filter(|c| !c.is_empty())
                        .ok_or("--counts takes one or more positive integers")?;
                    counts.sort_unstable();
                    counts.dedup();
                    args.counts = counts;
                }
                "--methods" => {
                    let v = values(&mut i);
                    let mut methods = Vec::new();
                    for name in &v {
                        let m = METHODS
                            .iter()
                            .find(|m| m.0 == name.as_str())
                            .ok_or_else(|| format!("--methods: unknown method {name:?}"))?;
                        methods.push(m.0);
                    }
                    if methods.is_empty() {
                        return Err("--methods takes one or more methods".into());
                    }
                    args.methods = methods;
                }
                "--allow-debug" => {
                    args.allow_debug = true;
                    i += 1;
                }
                "-h" | "--help" => {
                    return Err(format!(
                        "usage: timing [--trials N] [--warmup N] [--counts N...] \
                         [--methods {}] [--seed N] [--gap S] [--allow-debug]",
                        METHODS.map(|m| m.0).join(" ")
                    ));
                }
                other => return Err(format!("unknown argument {other:?} (try --help)")),
            }
        }
        if !args.gap.is_finite() || args.gap < 0.0 {
            return Err("--gap must be a non-negative number of seconds".into());
        }
        Ok(args)
    }
}
