//! `yeti-probe`: list the Blue Yeti's controls, then set and restore the input gain.
//!
//! ```text
//! cargo run -p yeti-engine --bin yeti-probe            # list + gain set/restore
//! cargo run -p yeti-engine --bin yeti-probe -- --list  # list only, change nothing
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn main() {
    let list_only = std::env::args().any(|a| a == "--list");

    #[cfg(target_os = "macos")]
    {
        println!("== CoreAudio HAL devices ==");
        match yeti_device::macos::list_devices() {
            Ok(devs) => {
                for d in devs {
                    let mark = if yeti_device::is_yeti_name(&d.name) {
                        "*"
                    } else {
                        " "
                    };
                    println!(
                        "{mark} id={:<4} in={:<5} out={:<5} usb={:<5} {:?} uid={:?}",
                        d.id, d.has_input, d.has_output, d.usb, d.name, d.uid
                    );
                }
            }
            Err(e) => println!("  error: {e}"),
        }
    }

    println!("\n== cpal devices ==");
    let (ins, outs) = yeti_engine::pipeline::list_device_names();
    println!("  inputs : {ins:?}");
    println!("  outputs: {outs:?}");

    println!("\n== Yeti controls ==");
    let dev = match yeti_device::open_yeti() {
        Ok(d) => d,
        Err(e) => {
            println!("open_yeti: {e}");
            std::process::exit(2);
        }
    };
    let st = dev.state().expect("state");
    print_state(&st);

    if list_only {
        return;
    }

    let events = Arc::new(AtomicUsize::new(0));
    let ev2 = events.clone();
    dev.set_event_callback(Some(Arc::new(move |e| {
        ev2.fetch_add(1, Ordering::SeqCst);
        if let yeti_types::DeviceEvent::Changed(s) = &e {
            println!(
                "  [event] Changed: gain={:?} dB muted={:?}",
                s.input_gain_db, s.input_muted
            );
        } else {
            println!("  [event] {e:?}");
        }
    })));

    let (Some(orig), Some((lo, hi))) = (st.input_gain_db, st.input_gain_range_db) else {
        println!("input gain not available; nothing to set");
        return;
    };
    // Move 3 dB towards the middle of the range so the target is always valid.
    let target = if orig - 3.0 >= lo {
        orig - 3.0
    } else {
        (orig + 3.0).min(hi)
    };
    println!("\n== Gain set/restore ==");
    println!("original gain : {orig:.2} dB (range {lo:.2}..={hi:.2})");
    let r = dev.set_input_gain_db(target);
    std::thread::sleep(Duration::from_millis(300));
    let after = dev.state().ok().and_then(|s| s.input_gain_db);
    println!("set {target:.2} dB -> {r:?}; read back {after:?}");
    let r2 = dev.set_input_gain_db(orig);
    std::thread::sleep(Duration::from_millis(300));
    let restored = dev.state().ok().and_then(|s| s.input_gain_db);
    println!("restore {orig:.2} dB -> {r2:?}; read back {restored:?}");
    println!("out-of-range check: {:?}", dev.set_input_gain_db(hi + 10.0));
    println!(
        "property-change events received: {}",
        events.load(Ordering::SeqCst)
    );
    let ok = r.is_ok()
        && r2.is_ok()
        && after.is_some_and(|a| (a - target).abs() < 0.6)
        && restored.is_some_and(|a| (a - orig).abs() < 0.01);
    println!(
        "RESULT: gain set/restore {}",
        if ok { "OK" } else { "FAILED" }
    );
    dev.set_event_callback(None);
    if !ok {
        std::process::exit(1);
    }
}

fn print_state(st: &yeti_types::DeviceState) {
    println!("connected            : {}", st.connected);
    println!("name / uid           : {:?} / {:?}", st.name, st.uid);
    println!(
        "input gain           : {:?} dB, range {:?}",
        st.input_gain_db, st.input_gain_range_db
    );
    println!("input muted          : {:?}", st.input_muted);
    println!("headphone volume     : {:?}", st.headphone_volume);
    println!("headphone muted      : {:?}", st.headphone_muted);
    println!(
        "sample rate          : {:?} (available {:?}, settable {})",
        st.sample_rate, st.available_sample_rates, st.sample_rate_settable
    );
}
