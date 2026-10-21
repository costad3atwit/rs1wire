//! Parsers for `w1_therm` sysfs content. Pure functions, no I/O.
//!
//! Parsing is strict: anything that doesn't match what the kernel prints is a
//! [`ParseErrorKind`], never a panic.

use std::time::Duration;

use crate::{BulkStatus, DeviceId, FAMILY_DS18B20, ParseErrorKind, Resolution, snippet};

/// One `w1_slave` read, as the kernel printed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Scratchpad {
    /// The 9 scratchpad bytes from the first line.
    pub bytes: [u8; 9],
    /// The kernel's own CRC-8 of `bytes[..8]` (`crc=XX`). Not `bytes[8]`.
    pub kernel_crc: u8,
    /// The kernel's verdict (`YES` or `NO`).
    pub kernel_ok: bool,
    /// `t=`, computed by the kernel from `bytes`, CRC-checked or not.
    pub millicelsius: i32,
}

/// Parses `w1_slave`, which the kernel prints as:
///
/// ```text
/// 72 01 4b 46 7f ff 0e 10 57 : crc=57 YES
/// 72 01 4b 46 7f ff 0e 10 57 t=23125
/// ```
///
/// The second line's bytes are the last scratchpad that passed CRC, not the
/// current one, so they are checked for shape and otherwise ignored.
pub(crate) fn parse_w1_slave(s: &str) -> Result<Scratchpad, ParseErrorKind> {
    let invalid = || ParseErrorKind::W1Slave(snippet(s));
    let mut lines = s.lines();
    let (Some(first), Some(second), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(invalid());
    };

    let (hex, verdict) = first.split_once(" : crc=").ok_or_else(invalid)?;
    let bytes = parse_hex_bytes(hex).ok_or_else(invalid)?;
    let (crc, yes_no) = verdict.split_once(' ').ok_or_else(invalid)?;
    let kernel_crc = parse_hex_byte(crc).ok_or_else(invalid)?;
    let kernel_ok = match yes_no {
        "YES" => true,
        "NO" => false,
        _ => return Err(invalid()),
    };

    let (cached, t) = second.rsplit_once(" t=").ok_or_else(invalid)?;
    parse_hex_bytes(cached).ok_or_else(invalid)?;
    let millicelsius = parse_int(t).ok_or_else(invalid)?;

    Ok(Scratchpad {
        bytes,
        kernel_crc,
        kernel_ok,
        millicelsius,
    })
}

/// Parses `therm_bulk_read`: `-1`, `0` or `1`.
pub(crate) fn parse_bulk_status(s: &str) -> Result<BulkStatus, ParseErrorKind> {
    match parse_int(s.trim_end()) {
        Some(-1) => Ok(BulkStatus::InProgress),
        Some(0) => Ok(BulkStatus::Idle),
        Some(1) => Ok(BulkStatus::CompleteUnread),
        _ => Err(ParseErrorKind::BulkStatus(snippet(s))),
    }
}

/// Parses a sensor's `resolution` file.
pub(crate) fn parse_resolution(s: &str) -> Result<Resolution, ParseErrorKind> {
    let invalid = || ParseErrorKind::Resolution(snippet(s));
    let bits = parse_int(s.trim_end())
        .and_then(|v| u8::try_from(v).ok())
        .ok_or_else(invalid)?;
    Resolution::try_from(bits).map_err(|_| invalid())
}

/// Parses a sensor's `conv_time` file (milliseconds).
pub(crate) fn parse_millis(s: &str) -> Result<Duration, ParseErrorKind> {
    parse_int(s.trim_end())
        .and_then(|v| u64::try_from(v).ok())
        .map(Duration::from_millis)
        .ok_or_else(|| ParseErrorKind::ConvTime(snippet(s)))
}

/// Parses a bus master's `w1_master_slaves`: one device name per line, or
/// `not found.` when the bus is empty. Only DS18B20s are returned. Lines that
/// aren't DS18B20 ids (other families, odd names) are skipped, so one foreign
/// device can't break discovery.
pub(crate) fn parse_slave_list(s: &str) -> Vec<DeviceId> {
    s.lines()
        .filter_map(|line| line.trim().parse::<DeviceId>().ok())
        .filter(|id| id.family() == FAMILY_DS18B20)
        .collect()
}

/// Exactly 9 two-digit hex bytes separated by single spaces.
fn parse_hex_bytes(s: &str) -> Option<[u8; 9]> {
    let mut bytes = [0u8; 9];
    let mut parts = s.split(' ');
    for byte in &mut bytes {
        *byte = parse_hex_byte(parts.next()?)?;
    }
    parts.next().is_none().then_some(bytes)
}

fn parse_hex_byte(s: &str) -> Option<u8> {
    // `from_str_radix` accepts a leading '+', so check the digits first.
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u8::from_str_radix(s, 16).ok()
}

/// A decimal integer as the kernel prints it with `%d`: optional `-`, then
/// digits. Overflow is `None`.
fn parse_int(s: &str) -> Option<i32> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str =
        "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=23125\n";

    #[test]
    fn w1_slave_good() {
        let sp = parse_w1_slave(GOOD).unwrap();
        assert_eq!(sp.bytes, [0x72, 0x01, 0x4b, 0x46, 0x7f, 0xff, 0x0e, 0x10, 0x57]);
        assert_eq!(sp.kernel_crc, 0x57);
        assert!(sp.kernel_ok);
        assert_eq!(sp.millicelsius, 23_125);
    }

    #[test]
    fn w1_slave_negative_and_crc_no() {
        let s = "5e ff 4b 46 7f ff 02 10 b6 : crc=b6 NO\n5e ff 4b 46 7f ff 02 10 b6 t=-10125\n";
        let sp = parse_w1_slave(s).unwrap();
        assert_eq!(sp.millicelsius, -10_125);
        assert!(!sp.kernel_ok);
    }

    #[test]
    fn w1_slave_without_trailing_newline() {
        assert!(parse_w1_slave(GOOD.trim_end()).is_ok());
    }

    #[test]
    fn w1_slave_rejects_malformed() {
        let bad = [
            "",
            "\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\nextra\n",
            "72 01 4b 46 7f ff 0e 10 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 57 00 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 zz : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 +7 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72  01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 57\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 MAYBE\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=5 YES\n72 01 4b 46 7f ff 0e 10 57 t=1\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=+1\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=12.5\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\n72 01 4b 46 7f ff 0e 10 57 t=99999999999\n",
            "72 01 4b 46 7f ff 0e 10 57 : crc=57 YES\nt=23125\n",
        ];
        for s in bad {
            assert!(
                matches!(parse_w1_slave(s), Err(ParseErrorKind::W1Slave(_))),
                "{s:?} parsed"
            );
        }
    }

    #[test]
    fn bulk_status_values() {
        assert_eq!(parse_bulk_status("-1\n").unwrap(), BulkStatus::InProgress);
        assert_eq!(parse_bulk_status("0\n").unwrap(), BulkStatus::Idle);
        assert_eq!(parse_bulk_status("1\n").unwrap(), BulkStatus::CompleteUnread);
        for bad in ["", "2\n", "trigger", "-\n", "+1\n"] {
            assert!(parse_bulk_status(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn resolution_values() {
        assert_eq!(parse_resolution("12\n").unwrap(), Resolution::Bits12);
        assert_eq!(parse_resolution("9\n").unwrap(), Resolution::Bits9);
        for bad in ["", "8\n", "13\n", "-5\n", "300\n", "twelve"] {
            assert!(parse_resolution(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn millis_values() {
        assert_eq!(parse_millis("750\n").unwrap(), Duration::from_millis(750));
        for bad in ["", "-1\n", "7.5\n"] {
            assert!(parse_millis(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn slave_list() {
        let ids = parse_slave_list("28-0123456789ab\n3a-000000000001\n28-00000000000f\n");
        let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["28-0123456789ab", "28-00000000000f"]);
        assert!(parse_slave_list("not found.\n").is_empty());
        assert!(parse_slave_list("").is_empty());
    }
}
