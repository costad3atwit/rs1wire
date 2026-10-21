//! Read DS18B20 1-Wire temperature sensors through the Linux kernel's w1
//! sysfs interface (`/sys/bus/w1/devices/`).
//!
//! This crate only consumes results the kernel's `w1_therm` driver has already
//! produced. It never implements 1-Wire timing in userspace. That does not make
//! it immune to timing problems: the kernel's `w1-gpio` master still bit-bangs
//! the bus in kernel space, and this crate inherits its jitter and bugs.
//!
//! Supported hardware is a Raspberry Pi 5 with DS18B20 sensors. Other boards
//! and other 1-Wire device families are out of scope.
//!
//! # Reading
//!
//! - [`Sensor::read`] reads one sensor's `w1_slave`, which makes the kernel run
//!   a conversion for that sensor (about 750 ms at 12 bits).
//! - [`BusMaster::bulk_convert`] converts every sensor on a bus at once and
//!   returns a [`BulkConversion`] that hands out each result once, while it is
//!   fresh. See its docs for how stale reads are prevented.
//!
//! # Integrity checks
//!
//! - **CRC.** Every read goes through `w1_slave`. The crate recomputes the
//!   Dallas/Maxim CRC-8 over the scratchpad and cross-checks it against the
//!   kernel's `crc=XX YES/NO`; any disagreement is [`Error::CrcMismatch`].
//!   When the kernel's own read fails (for example a CRC failure while
//!   fetching a bulk result), it returns an empty file instead of an error
//!   code, which this crate reports as [`Error::ReadFailed`].
//! - **85 °C.** The DS18B20 reports 85 °C after power-on until a conversion
//!   completes. By default `t=85000` is returned as [`Error::PowerOnReset`],
//!   which also rejects genuine 85 °C readings. See [`PowerOnResetPolicy`].

mod parse;

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use parse::Scratchpad;

/// Default location of the kernel's w1 devices directory.
pub const DEFAULT_ROOT: &str = "/sys/bus/w1/devices";

/// 1-Wire family code of the DS18B20.
pub const FAMILY_DS18B20: u8 = 0x28;

/// Longest piece of offending input kept in a [`ParseErrorKind`].
const MAX_SNIPPET: usize = 64;

/// Linux `ENODEV`, returned by a `trigger` write when no sensor on the bus
/// supports bulk reads.
const ENODEV: i32 = 19;

pub type Result<T> = std::result::Result<T, Error>;

/// Every failure this crate reports. Each cause has its own variant, so callers
/// never get a generic failure value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A kernel module this crate needs is not loaded.
    ///
    /// - `w1-gpio`: the sysfs root is missing or has no bus masters. The
    ///   device tree overlay may not be enabled.
    /// - `w1-therm`: a sensor directory exists but has no `w1_slave` file.
    #[error("kernel module {module} not loaded ({})", path.display())]
    ModuleNotLoaded { module: &'static str, path: PathBuf },

    /// No DS18B20 with this id exists under the sysfs root.
    #[error("device {id} not found")]
    DeviceNotFound { id: DeviceId },

    /// No bus master with this name exists under the sysfs root.
    #[error("bus master {name} not found")]
    BusNotFound { name: String },

    /// The bus master has no `therm_bulk_read` file (older kernel), or the
    /// kernel refused the trigger because no sensor on the bus supports it.
    #[error("bus master {bus} does not support therm_bulk_read")]
    BulkReadUnsupported { bus: String },

    /// The scratchpad failed CRC. Raised when the CRC-8 this crate computes
    /// over bytes 0..8 doesn't match byte 8, when the kernel printed `NO`, or
    /// when the kernel's printed CRC differs from this crate's.
    #[error(
        "CRC mismatch on {id}: computed {computed:#04x}, expected {expected:#04x} (kernel said {})",
        if *kernel_crc_ok { "YES" } else { "NO" }
    )]
    CrcMismatch {
        id: DeviceId,
        computed: u8,
        expected: u8,
        kernel_crc_ok: bool,
    },

    /// A conversion is running on the bus (`therm_bulk_read` is -1), so the
    /// kernel would return no data. Usually another process triggered it.
    #[error("bulk conversion in progress on {bus}")]
    ConversionInProgress { bus: String },

    /// A read was refused because it could return a stale value.
    #[error("stale bulk read of {id}: {reason}")]
    StaleBulkRead { id: DeviceId, reason: StaleReason },

    /// The kernel returned an empty `w1_slave`. It does this instead of
    /// returning an error code whenever its own read fails: a CRC failure
    /// while fetching a bulk result, a bus error, or a device that stopped
    /// responding. An all-zero scratchpad (which CRC-8 accepts) is reported
    /// here too, since it means nothing drove the bus.
    #[error("kernel returned no data for {id}")]
    ReadFailed { id: DeviceId },

    /// Kernel output or user input could not be parsed.
    #[error("parse error{}: {kind}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    Parse {
        path: Option<PathBuf>,
        kind: ParseErrorKind,
    },

    /// The sensor reported 85 °C, its power-on reset value, under
    /// [`PowerOnResetPolicy::Reject`].
    #[error("{id} reported 85 °C power-on reset value")]
    PowerOnReset { id: DeviceId },

    /// Any other I/O failure, e.g. `PermissionDenied` when writing `trigger`,
    /// `resolution` or `conv_time` without root, or `EIO` from a `conv_time`
    /// measurement.
    #[error("I/O error on {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Why a read was refused as possibly stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// This sensor was already read from this conversion. Reading it again
    /// would make the kernel start a fresh single-sensor conversion.
    AlreadyRead,
    /// The conversion is older than its freshness window. The kernel has no
    /// expiry of its own and would return the trigger-time value. The window
    /// is enforced by this crate.
    Expired { age: Duration, max_age: Duration },
    /// The sensor was not on the bus when the conversion was triggered.
    NotInConversion,
    /// An individual read was refused because the bus has unread bulk results
    /// (`therm_bulk_read` is 1), which the kernel would return instead of
    /// converting. See [`BusMaster::discard_pending`].
    BulkPending,
}

impl fmt::Display for StaleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRead => f.write_str("already read from this conversion"),
            Self::Expired { age, max_age } => {
                write!(f, "conversion is {age:?} old, limit is {max_age:?}")
            }
            Self::NotInConversion => f.write_str("sensor was not part of this conversion"),
            Self::BulkPending => f.write_str("bus has unread bulk conversion results"),
        }
    }
}

/// What failed to parse. Each variant holds the offending input, truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    DeviceId(String),
    W1Slave(String),
    Temperature(String),
    BulkStatus(String),
    Resolution(String),
    ConvTime(String),
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, content) = match self {
            Self::DeviceId(s) => ("device id", s),
            Self::W1Slave(s) => ("w1_slave content", s),
            Self::Temperature(s) => ("temperature", s),
            Self::BulkStatus(s) => ("therm_bulk_read status", s),
            Self::Resolution(s) => ("resolution", s),
            Self::ConvTime(s) => ("conv_time", s),
        };
        write!(f, "invalid {what} {content:?}")
    }
}

/// Truncates `s` to at most [`MAX_SNIPPET`] characters for error messages.
pub(crate) fn snippet(s: &str) -> String {
    s.chars().take(MAX_SNIPPET).collect()
}

/// A 1-Wire device id as the kernel names it, e.g. `28-0123456789ab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId {
    family: u8,
    serial: [u8; 6],
}

impl DeviceId {
    pub fn family(&self) -> u8 {
        self.family
    }

    /// Serial bytes in the order the kernel prints them.
    pub fn serial(&self) -> [u8; 6] {
        self.serial
    }
}

impl FromStr for DeviceId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let invalid = || Error::Parse {
            path: None,
            kind: ParseErrorKind::DeviceId(snippet(s)),
        };
        let (family, serial) = s.split_once('-').ok_or_else(invalid)?;
        // `from_str_radix` accepts a leading '+', so check the digits first.
        let all_hex = |part: &str| part.bytes().all(|b| b.is_ascii_hexdigit());
        if family.len() != 2 || serial.len() != 12 || !all_hex(family) || !all_hex(serial) {
            return Err(invalid());
        }
        let hex_byte = |pair: &str| u8::from_str_radix(pair, 16).map_err(|_| invalid());
        let mut bytes = [0u8; 6];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = hex_byte(&serial[i * 2..i * 2 + 2])?;
        }
        Ok(Self {
            family: hex_byte(family)?,
            serial: bytes,
        })
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}-", self.family)?;
        self.serial.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// A temperature in millidegrees Celsius, as the kernel reports it.
///
/// Values are not range-checked beyond parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Temperature(i32);

impl Temperature {
    pub const fn from_millicelsius(millicelsius: i32) -> Self {
        Self(millicelsius)
    }

    pub const fn millicelsius(&self) -> i32 {
        self.0
    }

    pub fn celsius(&self) -> f64 {
        f64::from(self.0) / 1000.0
    }
}

impl fmt::Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} °C", self.celsius())
    }
}

/// DS18B20 conversion resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Resolution {
    Bits9,
    Bits10,
    Bits11,
    Bits12,
}

impl Resolution {
    pub fn bits(&self) -> u8 {
        match self {
            Self::Bits9 => 9,
            Self::Bits10 => 10,
            Self::Bits11 => 11,
            Self::Bits12 => 12,
        }
    }

    /// Maximum conversion time from the DS18B20 datasheet.
    ///
    /// For documentation and defaults only. The kernel waits for its own
    /// `conv_time` (see [`Sensor::conv_time`]), not this value.
    pub fn nominal_conversion_time(&self) -> Duration {
        match self {
            Self::Bits9 => Duration::from_micros(93_750),
            Self::Bits10 => Duration::from_micros(187_500),
            Self::Bits11 => Duration::from_millis(375),
            Self::Bits12 => Duration::from_millis(750),
        }
    }
}

impl TryFrom<u8> for Resolution {
    type Error = Error;

    fn try_from(bits: u8) -> Result<Self> {
        match bits {
            9 => Ok(Self::Bits9),
            10 => Ok(Self::Bits10),
            11 => Ok(Self::Bits11),
            12 => Ok(Self::Bits12),
            _ => Err(Error::Parse {
                path: None,
                kind: ParseErrorKind::Resolution(bits.to_string()),
            }),
        }
    }
}

/// A value to write to a sensor's `conv_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvTime(u32);

impl ConvTime {
    /// Restore the kernel's default for the current resolution.
    pub const DEFAULT: Self = Self(0);

    /// Ask the kernel to measure the actual conversion time. The kernel
    /// refuses this with `EIO` (reported as [`Error::Io`]) while strong
    /// pull-up is enabled, which is the `w1_therm` module default.
    pub const MEASURE: Self = Self(1);

    /// A fixed conversion time in milliseconds. `None` below 2, since the
    /// kernel reads 0 and 1 as [`DEFAULT`](Self::DEFAULT) and
    /// [`MEASURE`](Self::MEASURE).
    pub const fn millis(ms: u32) -> Option<Self> {
        if ms < 2 { None } else { Some(Self(ms)) }
    }
}

/// How to treat a reading of exactly 85 °C (`t=85000`).
///
/// The DS18B20 holds 85 °C in its scratchpad after power-on until a conversion
/// completes, so 85 °C usually means the sensor lost power or was read too
/// early. A genuine 85 °C reading looks identical, though.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerOnResetPolicy {
    /// Return [`Error::PowerOnReset`]. Genuine 85 °C readings are discarded.
    #[default]
    Reject,
    /// Return 85 °C as a normal reading. Use when the measured range really
    /// includes 85 °C.
    Accept,
}

/// Options for individual and bulk reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadOptions {
    pub power_on_reset: PowerOnResetPolicy,
}

/// Options for [`BusMaster::bulk_convert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkOptions {
    /// How long after the conversion completes its results may be read.
    ///
    /// This is a policy this crate enforces, not a physical limit: the kernel
    /// keeps a bulk result indefinitely and returns it whenever the sensor is
    /// next read. Defaults to 1 s.
    pub max_age: Duration,
    pub read: ReadOptions,
}

impl Default for BulkOptions {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(1),
            read: ReadOptions::default(),
        }
    }
}

/// Entry point: the w1 sysfs root.
#[derive(Debug, Clone)]
pub struct W1 {
    root: PathBuf,
}

impl W1 {
    /// Uses [`DEFAULT_ROOT`].
    pub fn new() -> Self {
        Self::with_root(DEFAULT_ROOT)
    }

    /// Uses a different root, e.g. a fixture directory in tests.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All bus masters, sorted by name.
    pub fn bus_masters(&self) -> Result<Vec<BusMaster>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(self.not_loaded()),
            Err(e) => return Err(io_error(&self.root, e)),
        };
        let mut masters = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_error(&self.root, e))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with("w1_bus_master") {
                masters.push(BusMaster {
                    path: entry.path(),
                    name,
                });
            }
        }
        if masters.is_empty() {
            return Err(self.not_loaded());
        }
        masters.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(masters)
    }

    /// The bus master with this name, e.g. `w1_bus_master1`.
    pub fn bus(&self, name: &str) -> Result<BusMaster> {
        if !self.root.is_dir() {
            return Err(self.not_loaded());
        }
        let path = self.root.join(name);
        if !name.starts_with("w1_bus_master") || name.contains('/') || !path.is_dir() {
            return Err(Error::BusNotFound {
                name: name.to_owned(),
            });
        }
        Ok(BusMaster {
            name: name.to_owned(),
            path,
        })
    }

    /// All DS18B20s on all buses, sorted by id.
    pub fn sensors(&self) -> Result<Vec<Sensor>> {
        let mut sensors = Vec::new();
        for bus in self.bus_masters()? {
            sensors.extend(bus.sensors()?);
        }
        sensors.sort_by_key(Sensor::id);
        Ok(sensors)
    }

    /// The DS18B20 with this id.
    pub fn sensor(&self, id: DeviceId) -> Result<Sensor> {
        if !self.root.is_dir() {
            return Err(self.not_loaded());
        }
        if id.family() != FAMILY_DS18B20 {
            return Err(Error::DeviceNotFound { id });
        }
        let path = self.root.join(id.to_string());
        // `root/<id>` is a symlink into its bus master's directory.
        let real = match fs::canonicalize(&path) {
            Ok(real) => real,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Error::DeviceNotFound { id });
            }
            Err(e) => return Err(io_error(&path, e)),
        };
        let bus = real
            .parent()
            .ok_or(Error::DeviceNotFound { id })?
            .to_path_buf();
        Ok(Sensor { id, path, bus })
    }

    fn not_loaded(&self) -> Error {
        Error::ModuleNotLoaded {
            module: "w1-gpio",
            path: self.root.clone(),
        }
    }
}

impl Default for W1 {
    fn default() -> Self {
        Self::new()
    }
}

/// A w1 bus master, e.g. `w1_bus_master1`.
#[derive(Debug, Clone)]
pub struct BusMaster {
    name: String,
    path: PathBuf,
}

impl BusMaster {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The DS18B20s on this bus, from `w1_master_slaves`, sorted by id. Other
    /// device families are skipped.
    pub fn sensors(&self) -> Result<Vec<Sensor>> {
        let path = self.path.join("w1_master_slaves");
        let content = fs::read_to_string(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound && !self.path.is_dir() {
                Error::BusNotFound {
                    name: self.name.clone(),
                }
            } else {
                io_error(&path, e)
            }
        })?;
        let mut ids = parse::parse_slave_list(&content);
        ids.sort();
        Ok(ids.into_iter().map(|id| self.sensor_for(id)).collect())
    }

    /// The bus's `therm_bulk_read` status.
    pub fn bulk_status(&self) -> Result<BulkStatus> {
        bulk_status_at(&self.path)?.ok_or_else(|| Error::BulkReadUnsupported {
            bus: self.name.clone(),
        })
    }

    /// Converts every sensor on the bus at once.
    ///
    /// Blocks for about one conversion time: the kernel's `trigger` write
    /// sleeps for the longest `conv_time` on the bus before it returns, so
    /// when this returns the results are already waiting. Writing `trigger`
    /// needs root.
    ///
    /// The sensor list is taken from `w1_master_slaves` just before the
    /// trigger. Results left over from someone else's trigger are fine, since
    /// a new trigger replaces them.
    pub fn bulk_convert(&mut self, opts: BulkOptions) -> Result<BulkConversion<'_>> {
        let unread: BTreeSet<DeviceId> = self.sensors()?.iter().map(Sensor::id).collect();
        if self.bulk_status()? == BulkStatus::InProgress {
            return Err(Error::ConversionInProgress {
                bus: self.name.clone(),
            });
        }
        let path = self.path.join("therm_bulk_read");
        write_attr(&path, "trigger").map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(ENODEV) {
                Error::BulkReadUnsupported {
                    bus: self.name.clone(),
                }
            } else {
                io_error(&path, e)
            }
        })?;
        Ok(BulkConversion {
            completed_at: Instant::now(),
            max_age: opts.max_age,
            read_opts: opts.read,
            unread,
            done: BTreeSet::new(),
            bus: self,
        })
    }

    /// Reads and discards every sensor's pending bulk result, so the next
    /// individual read converts afresh. Use this to recover from
    /// [`StaleReason::BulkPending`] left behind by another process. Read
    /// failures are ignored, since the values are thrown away.
    pub fn discard_pending(&mut self) -> Result<()> {
        for sensor in self.sensors()? {
            let _ = sensor.read_scratchpad();
        }
        Ok(())
    }

    fn sensor_for(&self, id: DeviceId) -> Sensor {
        Sensor {
            id,
            path: self.path.join(id.to_string()),
            bus: self.path.clone(),
        }
    }
}

/// A DS18B20 on a bus.
#[derive(Debug, Clone)]
pub struct Sensor {
    id: DeviceId,
    path: PathBuf,
    bus: PathBuf,
}

impl Sensor {
    pub fn id(&self) -> DeviceId {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the temperature with default [`ReadOptions`].
    pub fn read(&self) -> Result<Temperature> {
        self.read_with(ReadOptions::default())
    }

    /// Reads the temperature. The kernel runs a conversion for this sensor,
    /// so this blocks for about one conversion time.
    ///
    /// Checks the bus's `therm_bulk_read` first: if a bulk result is pending,
    /// the kernel would return it instead of converting, so the read is
    /// refused with [`StaleReason::BulkPending`]. This check races with other
    /// processes: one can trigger between the check and the read.
    pub fn read_with(&self, opts: ReadOptions) -> Result<Temperature> {
        match bulk_status_at(&self.bus)? {
            Some(BulkStatus::InProgress) => return Err(self.in_progress()),
            Some(BulkStatus::CompleteUnread) => {
                return Err(Error::StaleBulkRead {
                    id: self.id,
                    reason: StaleReason::BulkPending,
                });
            }
            Some(BulkStatus::Idle) | None => {}
        }
        validate(self.id, self.read_scratchpad()?, opts)
    }

    /// The sensor's resolution.
    pub fn resolution(&self) -> Result<Resolution> {
        let (path, content) = self.read_attr("resolution")?;
        parse::parse_resolution(&content).map_err(|kind| Error::Parse {
            path: Some(path),
            kind,
        })
    }

    /// Sets the resolution. Needs root. The kernel also resets any
    /// `conv_time` override. The setting lives in the sensor's RAM and is lost
    /// on power-off; saving it to EEPROM is not supported.
    pub fn set_resolution(&self, resolution: Resolution) -> Result<()> {
        self.write_attr("resolution", &resolution.bits().to_string())
    }

    /// How long the kernel waits for a conversion on this sensor.
    pub fn conv_time(&self) -> Result<Duration> {
        let (path, content) = self.read_attr("conv_time")?;
        parse::parse_millis(&content).map_err(|kind| Error::Parse {
            path: Some(path),
            kind,
        })
    }

    /// Sets how long the kernel waits for a conversion. Needs root.
    pub fn set_conv_time(&self, conv_time: ConvTime) -> Result<()> {
        self.write_attr("conv_time", &conv_time.0.to_string())
    }

    /// Reads and parses `w1_slave` without the bus check or validation.
    fn read_scratchpad(&self) -> Result<Scratchpad> {
        let (path, content) = self.read_attr("w1_slave")?;
        if content.is_empty() {
            return Err(match bulk_status_at(&self.bus) {
                Ok(Some(BulkStatus::InProgress)) => self.in_progress(),
                _ => Error::ReadFailed { id: self.id },
            });
        }
        parse::parse_w1_slave(&content).map_err(|kind| Error::Parse {
            path: Some(path),
            kind,
        })
    }

    fn read_attr(&self, name: &str) -> Result<(PathBuf, String)> {
        let path = self.path.join(name);
        match fs::read_to_string(&path) {
            Ok(content) => Ok((path, content)),
            Err(e) => Err(self.attr_error(path, e)),
        }
    }

    fn write_attr(&self, name: &str, value: &str) -> Result<()> {
        let path = self.path.join(name);
        write_attr(&path, value).map_err(|e| self.attr_error(path, e))
    }

    /// A missing attribute means the sensor is gone, or, if its directory is
    /// still there, that `w1-therm` isn't loaded.
    fn attr_error(&self, path: PathBuf, e: io::Error) -> Error {
        if e.kind() != io::ErrorKind::NotFound {
            Error::Io { path, source: e }
        } else if self.path.is_dir() {
            Error::ModuleNotLoaded {
                module: "w1-therm",
                path,
            }
        } else {
            Error::DeviceNotFound { id: self.id }
        }
    }

    fn in_progress(&self) -> Error {
        Error::ConversionInProgress {
            bus: dir_name(&self.bus),
        }
    }
}

/// The kernel's `therm_bulk_read` status for a bus master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkStatus {
    /// `-1`: a conversion is in progress.
    InProgress,
    /// `1`: a conversion finished and some sensors have not been read.
    CompleteUnread,
    /// `0`: no bulk operation is pending.
    Idle,
}

/// The results of one bulk conversion. Each sensor can be read from it once,
/// within its freshness window.
///
/// The kernel returns the trigger-time value for any sensor not read promptly
/// and has no expiry of its own. This type refuses, with
/// [`Error::StaleBulkRead`]:
///
/// - a second read of the same sensor ([`StaleReason::AlreadyRead`]),
/// - a read more than `max_age` after the conversion completed
///   ([`StaleReason::Expired`]),
/// - a sensor that wasn't on the bus at trigger time
///   ([`StaleReason::NotInConversion`]).
///
/// It holds the bus mutably, so one bus handle has at most one conversion at a
/// time. When dropped, it reads and discards every sensor it didn't hand out
/// (about 10–15 ms each), so their results can't leak into a later
/// individual read.
///
/// [`read_all`](Self::read_all) consumes the conversion:
///
/// ```no_run
/// # fn demo(bus: &mut rs1wire::BusMaster) -> rs1wire::Result<()> {
/// let conversion = bus.bulk_convert(rs1wire::BulkOptions::default())?;
/// for (id, temperature) in conversion.read_all() {
///     println!("{id}: {temperature:?}");
/// }
/// # Ok(())
/// # }
/// ```
///
/// so reading it twice doesn't compile:
///
/// ```compile_fail
/// # fn demo(bus: &mut rs1wire::BusMaster) -> rs1wire::Result<()> {
/// let conversion = bus.bulk_convert(rs1wire::BulkOptions::default())?;
/// let first = conversion.read_all();
/// let second = conversion.read_all();
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct BulkConversion<'a> {
    bus: &'a mut BusMaster,
    completed_at: Instant,
    max_age: Duration,
    read_opts: ReadOptions,
    unread: BTreeSet<DeviceId>,
    done: BTreeSet<DeviceId>,
}

impl BulkConversion<'_> {
    pub fn bus(&self) -> &BusMaster {
        self.bus
    }

    /// When the `trigger` write returned, which is when the kernel considers
    /// the conversion complete.
    pub fn completed_at(&self) -> Instant {
        self.completed_at
    }

    pub fn max_age(&self) -> Duration {
        self.max_age
    }

    /// Sensors from this conversion that have not been read yet.
    pub fn unread(&self) -> impl Iterator<Item = &DeviceId> {
        self.unread.iter()
    }

    /// Reads one sensor's result from this conversion.
    pub fn read(&mut self, id: &DeviceId) -> Result<Temperature> {
        let id = *id;
        let stale = |reason| Error::StaleBulkRead { id, reason };
        if self.done.contains(&id) {
            return Err(stale(StaleReason::AlreadyRead));
        }
        if !self.unread.contains(&id) {
            return Err(stale(StaleReason::NotInConversion));
        }
        let age = self.completed_at.elapsed();
        if age >= self.max_age {
            // Left in `unread`, so `Drop` discards the stale value.
            return Err(stale(StaleReason::Expired {
                age,
                max_age: self.max_age,
            }));
        }
        self.unread.remove(&id);
        self.done.insert(id);
        let scratchpad = self.bus.sensor_for(id).read_scratchpad()?;
        validate(id, scratchpad, self.read_opts)
    }

    /// Reads every sensor not yet read, in id order, and consumes the
    /// conversion.
    pub fn read_all(mut self) -> Vec<(DeviceId, Result<Temperature>)> {
        let ids: Vec<DeviceId> = self.unread.iter().copied().collect();
        ids.into_iter().map(|id| (id, self.read(&id))).collect()
    }
}

impl Drop for BulkConversion<'_> {
    fn drop(&mut self) {
        for id in std::mem::take(&mut self.unread) {
            let _ = self.bus.sensor_for(id).read_scratchpad();
        }
    }
}

/// Checks a scratchpad and turns it into a temperature.
fn validate(id: DeviceId, scratchpad: Scratchpad, opts: ReadOptions) -> Result<Temperature> {
    if scratchpad.bytes == [0; 9] {
        return Err(Error::ReadFailed { id });
    }
    let computed = crc8(&scratchpad.bytes[..8]);
    let expected = scratchpad.bytes[8];
    if computed != expected || !scratchpad.kernel_ok || scratchpad.kernel_crc != computed {
        return Err(Error::CrcMismatch {
            id,
            computed,
            expected,
            kernel_crc_ok: scratchpad.kernel_ok,
        });
    }
    if scratchpad.millicelsius == 85_000 && opts.power_on_reset == PowerOnResetPolicy::Reject {
        return Err(Error::PowerOnReset { id });
    }
    Ok(Temperature::from_millicelsius(scratchpad.millicelsius))
}

/// `therm_bulk_read` in a bus master directory, or `None` if the file doesn't
/// exist (no bulk read support).
fn bulk_status_at(bus: &Path) -> Result<Option<BulkStatus>> {
    let path = bus.join("therm_bulk_read");
    match fs::read_to_string(&path) {
        Ok(content) => parse::parse_bulk_status(&content)
            .map(Some)
            .map_err(|kind| Error::Parse {
                path: Some(path),
                kind,
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(&path, e)),
    }
}

/// Writes a sysfs attribute in one `write`. Never creates the file.
fn write_attr(path: &Path, value: &str) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?
        .write_all(value.as_bytes())
}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Dallas/Maxim CRC-8 (polynomial x^8 + x^5 + x^4 + 1, reflected).
///
/// Over all 9 scratchpad bytes, a valid scratchpad gives 0.
fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |mut crc, &byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8C } else { crc >> 1 };
        }
        crc
    })
}

#[cfg(test)]
mod fixture {
    //! Fake sysfs trees. These are plain files, so they can't model the kernel
    //! consuming a bulk result on read; that is checked on hardware with
    //! `examples/smoke.rs`.

    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::W1;

    pub const GOOD: &str =
        "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=23125\n";
    pub const NEGATIVE: &str =
        "5e ff 4b 46 7f ff 02 10 b6 : crc=b6 YES\n5e ff 4b 46 7f ff 02 10 b6 t=-10125\n";
    pub const POWER_ON: &str =
        "50 05 4b 46 7f ff 0c 10 1c : crc=1c YES\n50 05 4b 46 7f ff 0c 10 1c t=85000\n";
    /// Byte 0 corrupted, but the "kernel" still says YES.
    pub const BAD_CRC: &str =
        "73 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=23187\n";
    pub const KERNEL_NO: &str =
        "72 01 4b 46 7f ff 0e 10 57 : crc=57 NO\n72 01 4b 46 7f ff 0e 10 57 t=23125\n";
    pub const KERNEL_CRC_DIFFERS: &str =
        "72 01 4b 46 7f ff 0e 10 57 : crc=58 YES\n72 01 4b 46 7f ff 0e 10 57 t=23125\n";
    pub const ALL_ZERO: &str =
        "00 00 00 00 00 00 00 00 00 : crc=00 YES\n00 00 00 00 00 00 00 00 00 t=0\n";

    pub struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        pub fn empty() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        pub fn root(&self) -> &Path {
            self.dir.path()
        }

        pub fn w1(&self) -> W1 {
            W1::with_root(self.root())
        }

        /// A bus master with these slaves, each with a `GOOD` `w1_slave`.
        /// `status` is the `therm_bulk_read` content, or `None` for no file.
        pub fn bus(&self, name: &str, status: Option<&str>, slaves: &[&str]) -> &Self {
            let bus = self.root().join(name);
            fs::create_dir_all(&bus).unwrap();
            if let Some(status) = status {
                fs::write(bus.join("therm_bulk_read"), status).unwrap();
            }
            let list: String = if slaves.is_empty() {
                "not found.\n".to_owned()
            } else {
                slaves.iter().map(|s| format!("{s}\n")).collect()
            };
            fs::write(bus.join("w1_master_slaves"), list).unwrap();
            for id in slaves {
                let dir = bus.join(id);
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("w1_slave"), GOOD).unwrap();
                std::os::unix::fs::symlink(&dir, self.root().join(id)).unwrap();
            }
            self
        }

        pub fn sensor_file(&self, bus: &str, id: &str) -> PathBuf {
            self.root().join(bus).join(id)
        }

        pub fn write(&self, bus: &str, id: &str, name: &str, content: &str) -> &Self {
            fs::write(self.sensor_file(bus, id).join(name), content).unwrap();
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    const A: &str = "28-00000000000a";
    const B: &str = "28-00000000000b";
    const DS2413: &str = "3a-000000000001";
    const BUS: &str = "w1_bus_master1";

    fn id(s: &str) -> DeviceId {
        s.parse().unwrap()
    }

    fn one_bus(status: Option<&str>) -> Fixture {
        let f = Fixture::empty();
        f.bus(BUS, status, &[A, B]);
        f
    }

    // --- Value types ---

    #[test]
    fn device_id_round_trip() {
        let id: DeviceId = "28-0123456789ab".parse().unwrap();
        assert_eq!(id.family(), FAMILY_DS18B20);
        assert_eq!(id.serial(), [0x01, 0x23, 0x45, 0x67, 0x89, 0xab]);
        assert_eq!(id.to_string(), "28-0123456789ab");
    }

    #[test]
    fn device_id_accepts_uppercase_and_prints_lowercase() {
        let id: DeviceId = "28-0123456789AB".parse().unwrap();
        assert_eq!(id.to_string(), "28-0123456789ab");
    }

    #[test]
    fn device_id_rejects_malformed() {
        for bad in [
            "",
            "28",
            "28_0123456789ab",
            "28-0123456789a",
            "28-0123456789abc",
            "2-0123456789ab",
            "28-0123456789ag",
            "+8-0123456789ab",
            "28-+123456789ab",
            "28-0123456789é",
            "w1_bus_master1",
        ] {
            let err = bad.parse::<DeviceId>().unwrap_err();
            assert!(
                matches!(err, Error::Parse { kind: ParseErrorKind::DeviceId(_), .. }),
                "{bad:?} gave {err:?}"
            );
        }
    }

    #[test]
    fn parse_error_snippet_is_truncated() {
        let long = "x".repeat(1000);
        let Err(Error::Parse { kind: ParseErrorKind::DeviceId(s), .. }) = long.parse::<DeviceId>()
        else {
            panic!("expected a device id parse error");
        };
        assert_eq!(s.len(), MAX_SNIPPET);
    }

    #[test]
    fn temperature_conversions() {
        let t = Temperature::from_millicelsius(23_125);
        assert_eq!(t.millicelsius(), 23_125);
        assert_eq!(t.celsius(), 23.125);

        let cold = Temperature::from_millicelsius(-10_062);
        assert_eq!(cold.celsius(), -10.062);
        assert!(cold < t);
    }

    #[test]
    fn resolution_try_from() {
        for bits in 9..=12u8 {
            assert_eq!(Resolution::try_from(bits).unwrap().bits(), bits);
        }
        for bits in [0u8, 8, 13, 255] {
            assert!(Resolution::try_from(bits).is_err());
        }
        assert_eq!(
            Resolution::Bits12.nominal_conversion_time(),
            Duration::from_millis(750)
        );
    }

    #[test]
    fn read_options_default_rejects_power_on_reset() {
        assert_eq!(ReadOptions::default().power_on_reset, PowerOnResetPolicy::Reject);
    }

    // From a w1_slave line: "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES".
    const SCRATCHPAD: [u8; 9] = [0x72, 0x01, 0x4b, 0x46, 0x7f, 0xff, 0x0e, 0x10, 0x57];

    #[test]
    fn crc8_valid_scratchpad() {
        assert_eq!(crc8(&SCRATCHPAD[..8]), 0x57);
        assert_eq!(crc8(&SCRATCHPAD), 0);
    }

    #[test]
    fn crc8_detects_flipped_bit() {
        let mut corrupted = SCRATCHPAD;
        corrupted[0] ^= 0x01;
        assert_ne!(crc8(&corrupted), 0);
    }

    // --- Discovery ---

    #[test]
    fn missing_root_is_module_not_loaded() {
        let w1 = W1::with_root("/nonexistent/rs1wire/root");
        for err in [
            w1.bus_masters().unwrap_err(),
            w1.sensors().unwrap_err(),
            w1.bus(BUS).unwrap_err(),
            w1.sensor(id(A)).unwrap_err(),
        ] {
            assert!(
                matches!(err, Error::ModuleNotLoaded { module: "w1-gpio", .. }),
                "{err:?}"
            );
        }
    }

    #[test]
    fn empty_root_is_module_not_loaded() {
        let f = Fixture::empty();
        assert!(matches!(
            f.w1().bus_masters(),
            Err(Error::ModuleNotLoaded { module: "w1-gpio", .. })
        ));
    }

    #[test]
    fn discovers_buses_and_sensors() {
        let f = Fixture::empty();
        f.bus("w1_bus_master2", Some("0\n"), &[B, DS2413])
            .bus(BUS, Some("0\n"), &[A]);
        let w1 = f.w1();

        let names: Vec<String> = w1.bus_masters().unwrap().iter().map(|b| b.name().to_owned()).collect();
        assert_eq!(names, [BUS, "w1_bus_master2"]);

        let ids: Vec<DeviceId> = w1.sensors().unwrap().iter().map(Sensor::id).collect();
        assert_eq!(ids, [id(A), id(B)]);
    }

    #[test]
    fn empty_bus_has_no_sensors() {
        let f = Fixture::empty();
        f.bus(BUS, Some("0\n"), &[]);
        assert!(f.w1().bus(BUS).unwrap().sensors().unwrap().is_empty());
    }

    #[test]
    fn unknown_bus_and_sensor() {
        let f = one_bus(Some("0\n"));
        let w1 = f.w1();
        assert!(matches!(w1.bus("w1_bus_master9"), Err(Error::BusNotFound { .. })));
        assert!(matches!(w1.bus("28-00000000000a"), Err(Error::BusNotFound { .. })));
        let missing = id("28-0000000000ff");
        assert!(matches!(w1.sensor(missing), Err(Error::DeviceNotFound { id }) if id == missing));
    }

    #[test]
    fn non_ds18b20_is_not_a_sensor() {
        let f = Fixture::empty();
        f.bus(BUS, Some("0\n"), &[DS2413]);
        assert!(matches!(f.w1().sensor(id(DS2413)), Err(Error::DeviceNotFound { .. })));
    }

    // --- Individual reads ---

    #[test]
    fn individual_read() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, B, "w1_slave", NEGATIVE);
        let w1 = f.w1();
        assert_eq!(w1.sensor(id(A)).unwrap().read().unwrap().millicelsius(), 23_125);
        assert_eq!(w1.sensor(id(B)).unwrap().read().unwrap().millicelsius(), -10_125);
    }

    #[test]
    fn individual_read_without_bulk_support() {
        let f = one_bus(None);
        assert!(f.w1().sensor(id(A)).unwrap().read().is_ok());
    }

    #[test]
    fn individual_read_refused_while_bulk_pending() {
        let f = one_bus(Some("1\n"));
        let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
        assert!(
            matches!(err, Error::StaleBulkRead { reason: StaleReason::BulkPending, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn individual_read_refused_while_converting() {
        let f = one_bus(Some("-1\n"));
        let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
        assert!(matches!(err, Error::ConversionInProgress { .. }), "{err:?}");
    }

    #[test]
    fn empty_w1_slave_is_read_failed() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "w1_slave", "");
        let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
        assert!(matches!(err, Error::ReadFailed { .. }), "{err:?}");
    }

    #[test]
    fn missing_w1_slave_is_w1_therm_not_loaded() {
        let f = one_bus(Some("0\n"));
        fs::remove_file(f.sensor_file(BUS, A).join("w1_slave")).unwrap();
        let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
        assert!(matches!(err, Error::ModuleNotLoaded { module: "w1-therm", .. }), "{err:?}");
    }

    #[test]
    fn removed_sensor_is_device_not_found() {
        let f = one_bus(Some("0\n"));
        let sensor = f.w1().sensor(id(A)).unwrap();
        fs::remove_dir_all(f.sensor_file(BUS, A)).unwrap();
        assert!(matches!(sensor.read(), Err(Error::DeviceNotFound { .. })));
    }

    #[test]
    fn malformed_w1_slave_is_parse_error() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "w1_slave", "garbage\n");
        let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
        assert!(
            matches!(err, Error::Parse { path: Some(_), kind: ParseErrorKind::W1Slave(_) }),
            "{err:?}"
        );
    }

    #[test]
    fn crc_failures() {
        for content in [BAD_CRC, KERNEL_NO, KERNEL_CRC_DIFFERS] {
            let f = one_bus(Some("0\n"));
            f.write(BUS, A, "w1_slave", content);
            let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
            assert!(matches!(err, Error::CrcMismatch { .. }), "{content:?} gave {err:?}");
        }
    }

    #[test]
    fn all_zero_scratchpad_is_read_failed() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "w1_slave", ALL_ZERO);
        let err = f.w1().sensor(id(A)).unwrap().read().unwrap_err();
        assert!(matches!(err, Error::ReadFailed { .. }), "{err:?}");
    }

    #[test]
    fn power_on_reset_policy() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "w1_slave", POWER_ON);
        let sensor = f.w1().sensor(id(A)).unwrap();
        assert!(matches!(sensor.read(), Err(Error::PowerOnReset { .. })));
        let accept = ReadOptions {
            power_on_reset: PowerOnResetPolicy::Accept,
        };
        assert_eq!(sensor.read_with(accept).unwrap().millicelsius(), 85_000);
    }

    // --- Bulk reads ---

    fn bulk_bus(f: &Fixture) -> BusMaster {
        f.w1().bus(BUS).unwrap()
    }

    #[test]
    fn bulk_writes_trigger_and_reads_all() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, B, "w1_slave", NEGATIVE);
        let mut bus = bulk_bus(&f);
        let results = bus.bulk_convert(BulkOptions::default()).unwrap().read_all();
        let temps: Vec<(DeviceId, i32)> = results
            .into_iter()
            .map(|(id, t)| (id, t.unwrap().millicelsius()))
            .collect();
        assert_eq!(temps, [(id(A), 23_125), (id(B), -10_125)]);
        let written = fs::read_to_string(f.root().join(BUS).join("therm_bulk_read")).unwrap();
        assert_eq!(written, "trigger");
    }

    #[test]
    fn bulk_double_read_is_rejected() {
        let f = one_bus(Some("0\n"));
        let mut bus = bulk_bus(&f);
        let mut conversion = bus.bulk_convert(BulkOptions::default()).unwrap();
        assert!(conversion.read(&id(A)).is_ok());
        let err = conversion.read(&id(A)).unwrap_err();
        assert!(
            matches!(err, Error::StaleBulkRead { reason: StaleReason::AlreadyRead, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn bulk_expired_read_is_rejected() {
        let f = one_bus(Some("0\n"));
        let mut bus = bulk_bus(&f);
        let opts = BulkOptions {
            max_age: Duration::ZERO,
            ..BulkOptions::default()
        };
        let mut conversion = bus.bulk_convert(opts).unwrap();
        let err = conversion.read(&id(A)).unwrap_err();
        assert!(
            matches!(err, Error::StaleBulkRead { reason: StaleReason::Expired { .. }, .. }),
            "{err:?}"
        );
        // Still unread, so Drop discards it.
        assert!(conversion.unread().any(|unread| *unread == id(A)));
    }

    #[test]
    fn bulk_read_of_sensor_not_in_conversion_is_rejected() {
        let f = one_bus(Some("0\n"));
        let mut bus = bulk_bus(&f);
        let mut conversion = bus.bulk_convert(BulkOptions::default()).unwrap();
        let err = conversion.read(&id("28-0000000000ff")).unwrap_err();
        assert!(
            matches!(err, Error::StaleBulkRead { reason: StaleReason::NotInConversion, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn bulk_validates_like_individual_reads() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "w1_slave", BAD_CRC).write(BUS, B, "w1_slave", "");
        let mut bus = bulk_bus(&f);
        let results = bus.bulk_convert(BulkOptions::default()).unwrap().read_all();
        assert!(matches!(results[0].1, Err(Error::CrcMismatch { .. })));
        assert!(matches!(results[1].1, Err(Error::ReadFailed { .. })));
    }

    #[test]
    fn bulk_without_support() {
        let f = one_bus(None);
        let mut bus = bulk_bus(&f);
        assert!(matches!(
            bus.bulk_convert(BulkOptions::default()),
            Err(Error::BulkReadUnsupported { .. })
        ));
        assert!(matches!(bus.bulk_status(), Err(Error::BulkReadUnsupported { .. })));
    }

    #[test]
    fn bulk_refused_while_converting() {
        let f = one_bus(Some("-1\n"));
        let mut bus = bulk_bus(&f);
        assert!(matches!(
            bus.bulk_convert(BulkOptions::default()),
            Err(Error::ConversionInProgress { .. })
        ));
    }

    #[test]
    fn bulk_status_is_parsed() {
        let f = one_bus(Some("1\n"));
        assert_eq!(bulk_bus(&f).bulk_status().unwrap(), BulkStatus::CompleteUnread);
    }

    // --- Configuration ---

    #[test]
    fn read_configuration() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "resolution", "11\n").write(BUS, A, "conv_time", "375\n");
        let sensor = f.w1().sensor(id(A)).unwrap();
        assert_eq!(sensor.resolution().unwrap(), Resolution::Bits11);
        assert_eq!(sensor.conv_time().unwrap(), Duration::from_millis(375));
    }

    #[test]
    fn write_configuration() {
        let f = one_bus(Some("0\n"));
        f.write(BUS, A, "resolution", "12\n").write(BUS, A, "conv_time", "750\n");
        let sensor = f.w1().sensor(id(A)).unwrap();
        let read_back = |name: &str| fs::read_to_string(f.sensor_file(BUS, A).join(name)).unwrap();

        sensor.set_resolution(Resolution::Bits9).unwrap();
        assert_eq!(read_back("resolution"), "9");

        for (conv_time, written) in [
            (ConvTime::DEFAULT, "0"),
            (ConvTime::MEASURE, "1"),
            (ConvTime::millis(200).unwrap(), "200"),
        ] {
            sensor.set_conv_time(conv_time).unwrap();
            assert_eq!(read_back("conv_time"), written);
        }
    }

    #[test]
    fn conv_time_millis_rejects_reserved_values() {
        assert_eq!(ConvTime::millis(0), None);
        assert_eq!(ConvTime::millis(1), None);
        assert!(ConvTime::millis(2).is_some());
    }

    #[test]
    fn missing_config_file_is_not_created() {
        let f = one_bus(Some("0\n"));
        let sensor = f.w1().sensor(id(A)).unwrap();
        assert!(matches!(
            sensor.set_resolution(Resolution::Bits9),
            Err(Error::ModuleNotLoaded { module: "w1-therm", .. })
        ));
        assert!(!f.sensor_file(BUS, A).join("resolution").exists());
    }
}
