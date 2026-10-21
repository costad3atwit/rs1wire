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
//! # Integrity checks
//!
//! - **CRC.** Reading `w1_slave` recomputes the Dallas/Maxim CRC-8 over the
//!   9 scratchpad bytes and cross-checks it against the kernel's `crc=YES/NO`.
//!   The bulk path reads the `temperature` file, which has no raw bytes, so
//!   there the crate relies on the kernel's own check. A kernel CRC failure
//!   surfaces as [`Error::Io`].
//! - **85 °C.** The DS18B20 reports 85 °C after power-on until a conversion
//!   completes. By default `t=85000` is returned as [`Error::PowerOnReset`],
//!   which also rejects genuine 85 °C readings. See [`PowerOnResetPolicy`].

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Default location of the kernel's w1 devices directory.
pub const DEFAULT_ROOT: &str = "/sys/bus/w1/devices";

/// 1-Wire family code of the DS18B20.
pub const FAMILY_DS18B20: u8 = 0x28;

/// Longest piece of offending input kept in a [`ParseErrorKind`].
const MAX_SNIPPET: usize = 64;

pub type Result<T> = std::result::Result<T, Error>;

/// Every failure this crate reports. Each cause has its own variant, so callers
/// never get a generic failure value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The w1 sysfs root is missing or has no bus masters. Usually `w1-gpio`
    /// or `w1-therm` is not loaded, or the device tree overlay is not enabled.
    #[error("w1 kernel modules not loaded: no bus masters under {}", root.display())]
    ModuleNotLoaded { root: PathBuf },

    /// No sensor with this id exists under the sysfs root.
    #[error("device {id} not found")]
    DeviceNotFound { id: DeviceId },

    /// No bus master with this name exists under the sysfs root.
    #[error("bus master {name} not found")]
    BusNotFound { name: String },

    /// The bus master has no `therm_bulk_read` file (older kernel).
    #[error("bus master {bus} does not support therm_bulk_read")]
    BulkReadUnsupported { bus: String },

    /// The CRC-8 recomputed from the `w1_slave` scratchpad bytes is invalid, or
    /// it disagrees with the kernel's `crc=YES/NO` verdict.
    ///
    /// Only reads of `w1_slave` can produce this. Bulk reads use the
    /// `temperature` file, which exposes no raw bytes, so a CRC failure there
    /// is detected by the kernel and reported as [`Error::Io`].
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

    /// `therm_bulk_read` still reported a conversion in progress (-1) when the
    /// wait timed out.
    #[error("bulk conversion still in progress on {bus}")]
    ConversionInProgress { bus: String },

    /// A bulk read was refused because it could return a stale value.
    #[error("stale bulk read of {id}: {reason}")]
    StaleBulkRead { id: DeviceId, reason: StaleReason },

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

    /// Any other I/O failure, e.g. `PermissionDenied` when writing `trigger`
    /// or `resolution` without root, or `EIO` when the kernel's CRC check
    /// fails on the `temperature` file.
    #[error("I/O error on {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Why a bulk read was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleReason {
    /// This sensor was already read from this conversion. The kernel would
    /// start a fresh single-sensor conversion, not return the bulk result.
    AlreadyRead,
    /// The conversion is older than its freshness window. The kernel has no
    /// expiry of its own and would return the trigger-time value. The window
    /// is enforced by this crate.
    Expired { age: Duration, max_age: Duration },
    /// The sensor was not on the bus when the conversion was triggered.
    NotInConversion,
}

impl fmt::Display for StaleReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRead => f.write_str("already read from this conversion"),
            Self::Expired { age, max_age } => {
                write!(f, "conversion is {age:?} old, limit is {max_age:?}")
            }
            Self::NotInConversion => f.write_str("sensor was not part of this conversion"),
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
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, content) = match self {
            Self::DeviceId(s) => ("device id", s),
            Self::W1Slave(s) => ("w1_slave content", s),
            Self::Temperature(s) => ("temperature", s),
            Self::BulkStatus(s) => ("therm_bulk_read status", s),
            Self::Resolution(s) => ("resolution", s),
        };
        write!(f, "invalid {what} {content:?}")
    }
}

/// Truncates `s` to at most [`MAX_SNIPPET`] characters for error messages.
fn snippet(s: &str) -> String {
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
    /// For documentation and defaults only. Waits are driven by
    /// `therm_bulk_read` status or the kernel's `conv_time`, not this value.
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
}

/// A temperature sensor on a bus.
#[derive(Debug, Clone)]
pub struct Sensor {
    id: DeviceId,
    path: PathBuf,
}

impl Sensor {
    pub fn id(&self) -> DeviceId {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The kernel's `therm_bulk_read` status for a bus master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkStatus {
    /// `-1`: a conversion is in progress.
    InProgress,
    /// `1`: the conversion finished and some sensors have not been read.
    CompleteUnread,
    /// `0`: no bulk operation is pending.
    Idle,
}

/// A bulk conversion that has been triggered but not yet waited on.
///
/// Holds the bus mutably, so a bus handle can have only one conversion in
/// flight at a time.
#[derive(Debug)]
pub struct PendingConversion<'a> {
    bus: &'a mut BusMaster,
    triggered_at: Instant,
}

impl PendingConversion<'_> {
    pub fn bus(&self) -> &BusMaster {
        self.bus
    }

    pub fn triggered_at(&self) -> Instant {
        self.triggered_at
    }
}

/// A completed bulk conversion. Each sensor can be read from it once, within
/// its freshness window.
///
/// The kernel returns the trigger-time value for any sensor not read promptly
/// and has no expiry of its own. `max_age` is a freshness window that this
/// crate enforces. Reading a sensor twice, reading after `max_age`, or reading
/// a sensor that was not on the bus is an [`Error::StaleBulkRead`].
#[derive(Debug)]
pub struct BulkConversion<'a> {
    bus: &'a mut BusMaster,
    completed_at: Instant,
    max_age: Duration,
    unread: BTreeSet<DeviceId>,
}

impl BulkConversion<'_> {
    pub fn bus(&self) -> &BusMaster {
        self.bus
    }

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
}

/// Dallas/Maxim CRC-8 (polynomial x^8 + x^5 + x^4 + 1, reflected).
///
/// Over all 9 scratchpad bytes, a valid scratchpad gives 0.
#[allow(dead_code)] // used by the w1_slave parser, coming next
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
mod tests {
    use super::*;

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
}
