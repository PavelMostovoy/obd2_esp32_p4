// ldproxy always prints "[ldproxy] Running ldproxy" to stderr; rustc reports linker output
// as `linker_messages` warnings. Pure noise, so silence it.
#![allow(linker_messages)]

mod board;
mod can;
mod ui;

use esp_idf_svc::hal::peripherals::Peripherals;

fn main() {
    // Required once at startup: links ESP-IDF runtime patches into the binary.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("obd2_esp32_p4 starting");

    let board = board::Board::init().expect("board init failed");

    let peripherals = Peripherals::take().expect("peripherals");
    let can_status = match can::Can::new(
        peripherals.can,
        peripherals.pins.gpio6,
        peripherals.pins.gpio7,
        can::Bitrate::K500,
        can::Mode::SelfTest,
    ) {
        Ok(can) => can::self_test::spawn(can),
        Err(e) => {
            log::error!("CAN init failed: {e}");
            std::sync::Arc::new(std::sync::Mutex::new(format!("CAN init failed: {e}")))
        }
    };

    ui::run(board, can_status);
}
