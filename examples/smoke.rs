//! Hardware smoke test for DS18B20s on the kernel's w1-gpio driver.
//!
//! Checks what the fixture tests can't: that the kernel really consumes a bulk
//! result on read, so dropping a partly read conversion leaves the bus idle,
//! and that an individual read is refused while someone else's bulk result is
//! pending.
//!
//! Bulk triggers need root:
//!
//! ```text
//! cargo build --example smoke && sudo ./target/debug/examples/smoke
//! ```

use std::error::Error;
use std::fs;
use std::process::ExitCode;
use std::time::Instant;

use rs1wire::{BulkOptions, BulkStatus, StaleReason, Temperature, W1};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool, Box<dyn Error>> {
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    println!("kernel {}", kernel.trim());

    let mut all_ok = true;
    for mut bus in W1::new().bus_masters()? {
        let sensors = bus.sensors()?;
        println!("{}: {} DS18B20(s)", bus.name(), sensors.len());
        let Some(first) = sensors.first() else {
            continue;
        };

        for sensor in &sensors {
            let start = Instant::now();
            let result = sensor.read();
            println!(
                "  {} individual: {} in {:?} (resolution {:?}, conv_time {:?})",
                sensor.id(),
                show(&result),
                start.elapsed(),
                sensor.resolution().ok().map(|r| r.bits()),
                sensor.conv_time().ok(),
            );
        }

        let start = Instant::now();
        let conversion = bus.bulk_convert(BulkOptions::default())?;
        let trigger = start.elapsed();
        let results = conversion.read_all();
        println!("  bulk: trigger {trigger:?}, total {:?}", start.elapsed());
        for (id, result) in &results {
            println!("    {id}: {}", show(result));
        }
        all_ok &= check(
            bus.bulk_status()? == BulkStatus::Idle,
            "bus idle after read_all",
        );

        {
            let mut conversion = bus.bulk_convert(BulkOptions::default())?;
            let _ = conversion.read(&first.id());
        }
        all_ok &= check(
            bus.bulk_status()? == BulkStatus::Idle,
            "bus idle after dropping a partly read conversion",
        );

        // Trigger behind the crate's back, as another process would.
        fs::write(bus.path().join("therm_bulk_read"), "trigger")?;
        let refused = matches!(
            first.read(),
            Err(rs1wire::Error::StaleBulkRead {
                reason: StaleReason::BulkPending,
                ..
            })
        );
        all_ok &= check(refused, "individual read refused while a bulk result is pending");

        bus.discard_pending()?;
        all_ok &= check(
            bus.bulk_status()? == BulkStatus::Idle && first.read().is_ok(),
            "individual read works after discard_pending",
        );
    }
    Ok(all_ok)
}

fn show(result: &rs1wire::Result<Temperature>) -> String {
    match result {
        Ok(t) => t.to_string(),
        Err(e) => format!("ERROR {e}"),
    }
}

fn check(ok: bool, what: &str) -> bool {
    println!("  [{}] {what}", if ok { "PASS" } else { "FAIL" });
    ok
}
