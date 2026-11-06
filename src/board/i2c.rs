//! Thin wrapper over ESP-IDF's new `i2c_master` driver.
//! (esp-idf-hal's `I2cDriver` wraps the legacy driver, which must not be mixed with the new one.)

use core::ptr;

use esp_idf_svc::sys::*;

const TIMEOUT_MS: i32 = 50;

pub struct I2cBus(i2c_master_bus_handle_t);

pub struct I2cDevice(i2c_master_dev_handle_t);

// The ESP-IDF driver handles are thread-safe (the driver takes a bus lock per transaction).
unsafe impl Send for I2cBus {}
unsafe impl Send for I2cDevice {}

impl I2cBus {
    pub fn new(port: i32, sda: i32, scl: i32) -> Result<Self, EspError> {
        let mut cfg = i2c_master_bus_config_t {
            i2c_port: port,
            sda_io_num: sda,
            scl_io_num: scl,
            glitch_ignore_cnt: 7,
            ..Default::default()
        };
        cfg.__bindgen_anon_1.clk_source = soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
        cfg.flags.set_enable_internal_pullup(1);

        let mut handle = ptr::null_mut();
        esp!(unsafe { i2c_new_master_bus(&cfg, &mut handle) })?;
        Ok(Self(handle))
    }

    /// True if a device ACKs at `addr` (7-bit).
    pub fn probe(&self, addr: u16) -> bool {
        esp!(unsafe { i2c_master_probe(self.0, addr, TIMEOUT_MS) }).is_ok()
    }

    pub fn device(&self, addr: u16, scl_speed_hz: u32) -> Result<I2cDevice, EspError> {
        let cfg = i2c_device_config_t {
            dev_addr_length: i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7,
            device_address: addr,
            scl_speed_hz,
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        esp!(unsafe { i2c_master_bus_add_device(self.0, &cfg, &mut handle) })?;
        Ok(I2cDevice(handle))
    }
}

impl I2cDevice {
    pub fn write(&self, data: &[u8]) -> Result<(), EspError> {
        esp!(unsafe { i2c_master_transmit(self.0, data.as_ptr(), data.len(), TIMEOUT_MS) })
    }

    pub fn write_read(&self, write: &[u8], read: &mut [u8]) -> Result<(), EspError> {
        esp!(unsafe {
            i2c_master_transmit_receive(
                self.0,
                write.as_ptr(),
                write.len(),
                read.as_mut_ptr(),
                read.len(),
                TIMEOUT_MS,
            )
        })
    }
}
