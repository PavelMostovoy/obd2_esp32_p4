// ldproxy always prints "[ldproxy] Running ldproxy" to stderr; rustc reports linker output
// as `linker_messages` warnings. Pure noise, so silence it.
#![allow(linker_messages)]

mod board;
mod ui;

fn main() {
    // Required once at startup: links ESP-IDF runtime patches into the binary.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("obd2_esp32_p4 starting");

    let board = board::Board::init().expect("board init failed");
    ui::run(board);
}
