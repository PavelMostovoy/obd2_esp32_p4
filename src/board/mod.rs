//! Everything specific to the M5Stack Tab5 hardware (ILI9881C + GT911 revision).
//!
//! Init order matters: I2C bus -> IO expanders (they hold LCD/touch reset) -> display -> touch.
//! Ported from M5Stack's Tab5 ESP-IDF BSP (`m5stack_tab5.c`), using the ESP-IDF C drivers via FFI.

mod display;
mod gt911;
mod i2c;
mod ili9881c_init;
mod io_expander;

pub use display::{Display, PANEL_HEIGHT, PANEL_WIDTH};
pub use gt911::{Touch, TouchPoint, TouchSample};

use esp_idf_svc::sys::EspError;

/// Internal "SYS" I2C bus (touch, IO expanders, codec, RTC, IMU, ...).
const SYS_I2C_PORT: i32 = 0;
const SYS_I2C_SDA: i32 = 31;
const SYS_I2C_SCL: i32 = 32;

pub struct Board {
    pub display: Display,
    pub touch: Touch,
    _io: io_expander::IoExpanders,
    /// Kept alive for the lifetime of the program; the devices above are attached to it.
    _sys_i2c: i2c::I2cBus,
}

impl Board {
    pub fn init() -> Result<Self, EspError> {
        let sys_i2c = i2c::I2cBus::new(SYS_I2C_PORT, SYS_I2C_SDA, SYS_I2C_SCL)?;
        let mut io = io_expander::init(&sys_i2c)?;
        let display = Display::new()?;
        let touch = Touch::new(&sys_i2c, &mut io)?;
        Ok(Self { display, touch, _io: io, _sys_i2c: sys_i2c })
    }
}
