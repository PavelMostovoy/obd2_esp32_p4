//! GT911 capacitive touch controller on the SYS I2C bus. Polled; INT (G23) is only used to wake it.
//! Coordinates are in the panel's native portrait orientation (x 0..720, y 0..1280).

use std::thread::sleep;
use std::time::Duration;

use esp_idf_svc::sys::*;

use super::i2c::{I2cBus, I2cDevice};
use super::io_expander::IoExpanders;

/// The address is latched from the INT level when reset is released: INT high -> 0x14 (what the
/// Tab5 BSP uses). 0x5D is tried as a fallback.
const ADDRESSES: [u16; 2] = [0x14, 0x5D];
const SPEED_HZ: u32 = 400_000;
const INT_GPIO: i32 = 23;

const REG_PRODUCT_ID: u16 = 0x8140;
const REG_X_MAX: u16 = 0x8048; // config: x max (LE u16), then y max
const REG_STATUS: u16 = 0x814E;
const REG_POINT1: u16 = 0x814F; // track id, x lo, x hi, y lo, y hi, size lo, size hi, reserved

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TouchSample {
    /// The controller has no new sample since the last poll (keep the previous state).
    NoNewData,
    /// New sample: no finger on the screen.
    Released,
    /// New sample: first finger position.
    Pressed(TouchPoint),
}

pub struct Touch {
    dev: I2cDevice,
}

impl Touch {
    pub fn new(bus: &I2cBus, io: &mut IoExpanders) -> Result<Self, EspError> {
        hardware_reset(io)?;
        let addr = ADDRESSES
            .into_iter()
            .find(|&a| bus.probe(a))
            .ok_or_else(|| {
                log::error!("GT911 not found at {ADDRESSES:#04x?}");
                EspError::from_infallible::<ESP_ERR_NOT_FOUND>()
            })?;
        let touch = Self { dev: bus.device(addr, SPEED_HZ)? };
        // The M5 BSP also issues `esp_lcd_touch_exit_sleep` right after creating the driver.
        wake()?;

        let mut id = [0u8; 4];
        touch.read(REG_PRODUCT_ID, &mut id)?;
        let mut res = [0u8; 4];
        touch.read(REG_X_MAX, &mut res)?;
        log::info!(
            "Touch GT{} at {addr:#04x}, configured {}x{}",
            core::str::from_utf8(&id).unwrap_or("?").trim_end_matches('\0'),
            u16::from_le_bytes([res[0], res[1]]),
            u16::from_le_bytes([res[2], res[3]]),
        );
        Ok(touch)
    }

    fn read(&self, reg: u16, buf: &mut [u8]) -> Result<(), EspError> {
        self.dev.write_read(&reg.to_be_bytes(), buf)
    }

    fn write(&self, reg: u16, value: u8) -> Result<(), EspError> {
        let [hi, lo] = reg.to_be_bytes();
        self.dev.write(&[hi, lo, value])
    }

    /// Polls the controller. Only the first touch point is reported.
    pub fn poll(&mut self) -> Result<TouchSample, EspError> {
        let mut status = [0u8];
        self.read(REG_STATUS, &mut status)?;
        if status[0] & 0x80 == 0 {
            return Ok(TouchSample::NoNewData);
        }
        let count = status[0] & 0x0F;
        let sample = if count > 0 {
            let mut p = [0u8; 8];
            self.read(REG_POINT1, &mut p)?;
            TouchSample::Pressed(TouchPoint {
                x: u16::from_le_bytes([p[1], p[2]]),
                y: u16::from_le_bytes([p[3], p[4]]),
            })
        } else {
            TouchSample::Released
        };
        // Acknowledge the sample so the controller can produce the next one.
        self.write(REG_STATUS, 0)?;
        Ok(sample)
    }
}

fn set_int(mode: gpio_mode_t, level: u32) -> Result<(), EspError> {
    let cfg = gpio_config_t { pin_bit_mask: 1u64 << INT_GPIO, mode, ..Default::default() };
    esp!(unsafe { gpio_config(&cfg) })?;
    if mode != gpio_mode_t_GPIO_MODE_INPUT {
        esp!(unsafe { gpio_set_level(INT_GPIO, level) })?;
    }
    Ok(())
}

/// GT911 power-on sequence (datasheet / Linux `goodix` driver). Without it, after a *software*
/// reset of the ESP32 the GT911 still answers on I2C but never reports touches; only a power cycle
/// used to recover it.
///  1. INT high (selects address 0x14), RST low >= 100 us
///  2. RST high, keep INT for >= 5 ms (address latched)
///  3. INT low for 50 ms ("INT sync")
///  4. INT released as floating input
fn hardware_reset(io: &mut IoExpanders) -> Result<(), EspError> {
    set_int(gpio_mode_t_GPIO_MODE_OUTPUT, 1)?;
    io.set_touch_reset(true)?;
    sleep(Duration::from_millis(1));
    io.set_touch_reset(false)?;
    sleep(Duration::from_millis(10));
    set_int(gpio_mode_t_GPIO_MODE_OUTPUT, 0)?;
    sleep(Duration::from_millis(50));
    set_int(gpio_mode_t_GPIO_MODE_INPUT, 0)?;
    sleep(Duration::from_millis(50));
    Ok(())
}

/// Wakes the GT911 from sleep: drive INT high for 5 ms, then release it (open-drain, line idles high).
/// Same as `esp_lcd_touch_gt911_exit_sleep` in Espressif's driver.
fn wake() -> Result<(), EspError> {
    let mut cfg = gpio_config_t {
        pin_bit_mask: 1u64 << INT_GPIO,
        mode: gpio_mode_t_GPIO_MODE_OUTPUT,
        ..Default::default()
    };
    esp!(unsafe { gpio_config(&cfg) })?;
    esp!(unsafe { gpio_set_level(INT_GPIO, 1) })?;
    sleep(Duration::from_millis(5));
    cfg.mode = gpio_mode_t_GPIO_MODE_OUTPUT_OD;
    esp!(unsafe { gpio_config(&cfg) })?;
    // Give the controller time to resume scanning before the first poll.
    sleep(Duration::from_millis(50));
    Ok(())
}
