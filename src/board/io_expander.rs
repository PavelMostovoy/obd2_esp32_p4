//! Two PI4IOE5V6408 8-bit IO expanders on the SYS I2C bus. Register values follow the M5 Tab5 BSP,
//! except that every optional power rail stays OFF (speaker amp, EXT 5V, Wi-Fi module, USB-A 5V).

use std::thread::sleep;
use std::time::Duration;

use esp_idf_svc::sys::EspError;

use super::i2c::{I2cBus, I2cDevice};

const EXPANDER1_ADDR: u16 = 0x43;
const EXPANDER2_ADDR: u16 = 0x44;
const SPEED_HZ: u32 = 400_000;

const REG_CHIP_RESET: u8 = 0x01;
const REG_IO_DIR: u8 = 0x03; // 1 = output
const REG_OUT_SET: u8 = 0x05;
const REG_OUT_H_IM: u8 = 0x07; // 1 = output high-impedance
const REG_IN_DEF_STA: u8 = 0x09;
const REG_PULL_EN: u8 = 0x0B;
const REG_PULL_SEL: u8 = 0x0D; // 1 = pull-up
const REG_INT_MASK: u8 = 0x11; // 1 = interrupt disabled

fn write_reg(dev: &I2cDevice, reg: u8, value: u8) -> Result<(), EspError> {
    dev.write(&[reg, value])
}

fn reset(dev: &I2cDevice) -> Result<(), EspError> {
    write_reg(dev, REG_CHIP_RESET, 0xFF)?;
    // Reading the reset register back clears the reset/interrupt flag.
    let mut id = [0u8];
    dev.write_read(&[REG_CHIP_RESET], &mut id)?;
    log::info!("PI4IOE5V6408 reset, device-id reg = {:#04x}", id[0]);
    Ok(())
}

/// Expander #1 bit for the touch controller reset line (active low).
const E1_TP_RST: u8 = 1 << 5;

/// Handle to the outputs that other drivers need after init.
pub struct IoExpanders {
    e1: I2cDevice,
    e1_out: u8,
}

impl IoExpanders {
    /// Drives the GT911 reset line (TP_RST, expander #1 P5). `true` = held in reset.
    pub fn set_touch_reset(&mut self, asserted: bool) -> Result<(), EspError> {
        if asserted {
            self.e1_out &= !E1_TP_RST;
        } else {
            self.e1_out |= E1_TP_RST;
        }
        write_reg(&self.e1, REG_OUT_SET, self.e1_out)
    }
}

/// Configures both expanders and pulses LCD reset. Afterwards the LCD and touch controller are out of reset.
pub fn init(bus: &I2cBus) -> Result<IoExpanders, EspError> {
    // #1 (0x43): P0 antenna select, P1 SPK_EN, P2 EXT5V_EN, P4 LCD_RST, P5 TP_RST, P6 CAM_RST, P7 HP_DET (input)
    let e1 = bus.device(EXPANDER1_ADDR, SPEED_HZ)?;
    reset(&e1)?;
    write_reg(&e1, REG_PULL_SEL, 0b0111_1111)?;
    write_reg(&e1, REG_PULL_EN, 0b0111_1111)?;
    // High: TP_RST, CAM_RST. Low: antenna internal, speaker off, EXT5V off (PwrCAN also feeds that rail),
    // LCD_RST latched low.
    let e1_out = 0b0110_0000;
    write_reg(&e1, REG_OUT_SET, e1_out)?;
    write_reg(&e1, REG_OUT_H_IM, 0b0000_0000)?;
    // P0..P6 outputs: LCD_RST is now driven low (reset asserted).
    write_reg(&e1, REG_IO_DIR, 0b0111_1111)?;
    sleep(Duration::from_millis(10));
    // The panel side has no pull-up on LCD_RST and must not see 3.3 V push-pull, so release reset
    // by turning P4 into an input with pull-up (same as the BSP).
    write_reg(&e1, REG_IO_DIR, 0b0110_1111)?;
    sleep(Duration::from_millis(50));

    // #2 (0x44): P0 WLAN_PWR_EN, P3 USB5V_EN, P4 PWROFF_PULSE, P5 nCHG_QC_EN, P6 CHG_STAT (input), P7 CHG_EN
    let e2 = bus.device(EXPANDER2_ADDR, SPEED_HZ)?;
    reset(&e2)?;
    write_reg(&e2, REG_IO_DIR, 0b1011_1001)?;
    write_reg(&e2, REG_OUT_H_IM, 0b0000_0110)?;
    write_reg(&e2, REG_PULL_SEL, 0b1011_1001)?;
    write_reg(&e2, REG_PULL_EN, 0b1111_1001)?;
    write_reg(&e2, REG_IN_DEF_STA, 0b0100_0000)?;
    write_reg(&e2, REG_INT_MASK, 0b1011_1111)?;
    // All low: Wi-Fi module off, USB-A 5V off, no power-off pulse, charger as BSP default.
    write_reg(&e2, REG_OUT_SET, 0b0000_0000)?;

    Ok(IoExpanders { e1, e1_out })
}
