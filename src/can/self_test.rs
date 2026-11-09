//! Bench self-test: once per second send a frame with self-reception and check that it comes back
//! through the PwrCAN transceiver. Needs the PwrCAN powered (9-24 V) and its 120R termination ON.
//! Runs in its own thread; publishes a one-line status for the UI.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{BusState, Can, CanFrame};

const TEST_ID: u16 = 0x123;
const PERIOD: Duration = Duration::from_secs(1);

/// Starts the self-test thread. Returns the shared status text (updated every second).
pub fn spawn(mut can: Can) -> Arc<Mutex<String>> {
    let status = Arc::new(Mutex::new(String::from("CAN self-test: starting")));
    let shared = status.clone();
    thread::Builder::new()
        .name("can-selftest".into())
        .stack_size(8 * 1024)
        .spawn(move || run(&mut can, &shared))
        .expect("spawn CAN self-test thread");
    status
}

fn run(can: &mut Can, status: &Mutex<String>) {
    let (mut sent, mut echoed, mut send_errors) = (0u32, 0u32, 0u32);
    for seq in 0u32.. {
        let payload = seq.to_be_bytes();
        let frame = CanFrame::new_standard(TEST_ID, &[0xCA, 0xFE, payload[0], payload[1], payload[2], payload[3]])
            .expect("valid test frame");

        let send_result = can.send(&frame, 50);
        match &send_result {
            Ok(()) => sent += 1,
            Err(_) => send_errors += 1,
        }

        // Look for our own frame coming back (self-reception). Skip anything else.
        let mut echo = false;
        if send_result.is_ok() {
            while let Ok(Some(rx)) = can.receive(100) {
                if rx == frame {
                    echo = true;
                    break;
                }
                log::info!("CAN rx other: id={:#05x} data={:02x?}", rx.id, rx.data.as_slice());
            }
        }
        if echo {
            echoed += 1;
        }

        let line = match can.status() {
            Ok(st) => {
                if st.state != BusState::Running {
                    if let Err(e) = can.recover() {
                        log::warn!("CAN recover failed: {e}");
                    }
                }
                format!(
                    "CAN self-test 500k: sent {sent}, echoed {echoed}, send errors {send_errors} | {:?} TEC {} REC {} bus errors {}",
                    st.state, st.tx_error_counter, st.rx_error_counter, st.bus_errors
                )
            }
            Err(e) => format!("CAN status error: {e}"),
        };
        log::info!(
            "#{seq} {} -> {}",
            match &send_result {
                Ok(()) => "sent".to_string(),
                Err(e) => format!("send failed ({e})"),
            },
            if echo { "echo OK" } else { "no echo" }
        );
        log::info!("{line}");
        if let Ok(mut s) = status.lock() {
            *s = line;
        }
        thread::sleep(PERIOD);
    }
}
