//! 5" 720x1280 (portrait-native) ILI9881C panel over 2-lane MIPI-DSI, plus the PWM backlight.
//! Port of the M5 Tab5 BSP display setup and Espressif's `esp_lcd_ili9881c` init flow.

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::Duration;

use esp_idf_svc::sys::*;

use super::ili9881c_init::INIT_CMDS;

/// Native (portrait) panel resolution.
pub const PANEL_WIDTH: usize = 720;
pub const PANEL_HEIGHT: usize = 1280;

const BACKLIGHT_GPIO: i32 = 22;
const BACKLIGHT_CHANNEL: ledc_channel_t = ledc_channel_t_LEDC_CHANNEL_1;
const BACKLIGHT_TIMER: ledc_timer_t = ledc_timer_t_LEDC_TIMER_0;
const BACKLIGHT_MAX_DUTY: u32 = (1 << 12) - 1; // 12-bit resolution

/// LDO_VO3 powers the MIPI D-PHY (VDD_MIPI_DPHY).
const DSI_PHY_LDO_CHANNEL: i32 = 3;
const DSI_PHY_LDO_MV: i32 = 2500;
const DSI_LANES: u8 = 2;
const DSI_LANE_MBPS: f32 = 730.0;
const DPI_CLOCK_MHZ: f32 = 60.0;

// Standard MIPI DCS commands.
const CMD_SWRESET: u8 = 0x01;
const CMD_SLPOUT: u8 = 0x11;
const CMD_MADCTL: u8 = 0x36;
const CMD_COLMOD: u8 = 0x3A;
// ILI9881C-specific.
const ILI_CMD_PAGE_SELECT: u8 = 0xFF;
const ILI_PAD_CONTROL: u8 = 0xB7;
const ILI_DSI_2_LANE: u8 = 0x03;
const COLMOD_RGB565: u8 = 0x55;

/// How long [`Display::present`] waits for the buffer switch (one frame is ~16 ms at 60 Hz).
const SWAP_TIMEOUT: Duration = Duration::from_millis(50);

/// Incremented from the DSI DMA interrupt each time a new frame starts being scanned out.
static FRAMES_STARTED: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn on_frame_buf_complete(
    _panel: esp_lcd_panel_handle_t,
    _edata: *mut esp_lcd_dpi_panel_event_data_t,
    _user_ctx: *mut c_void,
) -> bool {
    FRAMES_STARTED.fetch_add(1, Ordering::Release);
    false // no higher-priority task woken
}

/// Double-buffered: the DMA scans out one frame buffer while we draw into the other ("back") one.
/// [`Display::present`] makes the back buffer visible at the next frame boundary, so the panel never
/// shows a half-drawn frame.
pub struct Display {
    panel: esp_lcd_panel_handle_t,
    frame_buffers: [*mut u16; 2],
    back: usize,
}

// Handles are only used from the UI thread; the struct is moved there once.
unsafe impl Send for Display {}

impl Display {
    pub fn new() -> Result<Self, EspError> {
        backlight_init()?;

        let mut phy_ldo = ptr::null_mut();
        let ldo_cfg = esp_ldo_channel_config_t {
            chan_id: DSI_PHY_LDO_CHANNEL,
            voltage_mv: DSI_PHY_LDO_MV,
            ..Default::default()
        };
        esp!(unsafe { esp_ldo_acquire_channel(&ldo_cfg, &mut phy_ldo) })?;

        let mut dsi_bus = ptr::null_mut();
        let bus_cfg = esp_lcd_dsi_bus_config_t {
            bus_id: 0,
            num_data_lanes: DSI_LANES,
            // Chip rev < 3.0: only PLL_F20M / PLL_F25M / RC_FAST exist. `..._DEFAULT` (XTAL) is rev >= 3.0
            // only and aborts in the HAL. The C macro `MIPI_DSI_PHY_CLK_SRC_DEFAULT` maps to this one.
            phy_clk_src: soc_periph_mipi_dsi_phy_pllref_clk_src_t_MIPI_DSI_PHY_PLLREF_CLK_SRC_DEFAULT_LEGACY,
            lane_bit_rate_mbps: DSI_LANE_MBPS,
            ..Default::default()
        };
        esp!(unsafe { esp_lcd_new_dsi_bus(&bus_cfg, &mut dsi_bus) })?;

        // DBI (command mode) channel for sending LCD commands.
        let mut io = ptr::null_mut();
        let dbi_cfg = esp_lcd_dbi_io_config_t {
            virtual_channel: 0,
            lcd_cmd_bits: 8,
            lcd_param_bits: 8,
        };
        esp!(unsafe { esp_lcd_new_panel_io_dbi(dsi_bus, &dbi_cfg, &mut io) })?;

        // DPI (video mode) panel: streams the frame buffer (in PSRAM) to the panel continuously.
        let mut panel = ptr::null_mut();
        let dpi_cfg = esp_lcd_dpi_panel_config_t {
            virtual_channel: 0,
            dpi_clk_src: soc_periph_mipi_dsi_dpi_clk_src_t_MIPI_DSI_DPI_CLK_SRC_DEFAULT,
            dpi_clock_freq_mhz: DPI_CLOCK_MHZ,
            pixel_format: lcd_color_rgb_pixel_format_t_LCD_COLOR_PIXEL_FORMAT_RGB565,
            num_fbs: 2,
            video_timing: esp_lcd_video_timing_t {
                h_size: PANEL_WIDTH as u32,
                v_size: PANEL_HEIGHT as u32,
                hsync_pulse_width: 40,
                hsync_back_porch: 140,
                hsync_front_porch: 40,
                vsync_pulse_width: 4,
                vsync_back_porch: 20,
                vsync_front_porch: 20,
            },
            ..Default::default()
        };
        esp!(unsafe { esp_lcd_new_panel_dpi(dsi_bus, &dpi_cfg, &mut panel) })?;

        ili9881c_init(io)?;

        let mut cbs = esp_lcd_dpi_panel_event_callbacks_t {
            on_color_trans_done: None,
            __bindgen_anon_1: Default::default(),
            on_vsync: None,
        };
        cbs.__bindgen_anon_1.on_frame_buf_complete = Some(on_frame_buf_complete);
        esp!(unsafe { esp_lcd_dpi_panel_register_event_callbacks(panel, &cbs, ptr::null_mut()) })?;

        // Starts DPI video streaming.
        esp!(unsafe { esp_lcd_panel_init(panel) })?;

        let mut fb0: *mut c_void = ptr::null_mut();
        let mut fb1: *mut c_void = ptr::null_mut();
        esp!(unsafe {
            esp_lcd_dpi_panel_get_frame_buffer(
                panel,
                2,
                &mut fb0 as *mut *mut c_void,
                &mut fb1 as *mut *mut c_void,
            )
        })?;

        let mut display = Self { panel, frame_buffers: [fb0.cast(), fb1.cast()], back: 0 };
        // Clear both buffers; presenting both leaves buffer 1 on screen and buffer 0 as the back buffer.
        for _ in 0..2 {
            display.back_buffer().fill(0);
            display.present(0, PANEL_HEIGHT)?;
        }
        display.set_brightness(100)?;
        log::info!("Display ready: {PANEL_WIDTH}x{PANEL_HEIGHT} RGB565");
        Ok(display)
    }

    /// The buffer to draw the next frame into: RGB565, row-major, `PANEL_WIDTH` pixels per row.
    /// It holds the frame from two `present` calls ago (needed for Slint's `SwappedBuffers` mode).
    pub fn back_buffer(&mut self) -> &mut [u16] {
        unsafe { core::slice::from_raw_parts_mut(self.frame_buffers[self.back], PANEL_WIDTH * PANEL_HEIGHT) }
    }

    /// Shows the back buffer: writes rows `y_start..y_end` from the CPU cache back to PSRAM, switches
    /// the DMA to this buffer at the next frame boundary, and waits for that switch. Afterwards the
    /// other buffer is the back buffer and is no longer read by the DMA.
    pub fn present(&mut self, y_start: usize, y_end: usize) -> Result<(), EspError> {
        let fb = self.frame_buffers[self.back];
        // A pointer inside the driver's own frame buffer: cache write-back + buffer switch, no copy.
        esp!(unsafe {
            esp_lcd_panel_draw_bitmap(
                self.panel,
                0,
                y_start as i32,
                PANEL_WIDTH as i32,
                y_end as i32,
                fb as *const c_void,
            )
        })?;
        let seen = FRAMES_STARTED.load(Ordering::Acquire);
        let start = std::time::Instant::now();
        while FRAMES_STARTED.load(Ordering::Acquire) == seen {
            if start.elapsed() > SWAP_TIMEOUT {
                log::warn!("display: no frame-start event within {SWAP_TIMEOUT:?}");
                break;
            }
            sleep(Duration::from_millis(1));
        }
        self.back ^= 1;
        Ok(())
    }

    pub fn set_brightness(&self, percent: u8) -> Result<(), EspError> {
        let duty = BACKLIGHT_MAX_DUTY * percent.min(100) as u32 / 100;
        esp!(unsafe { ledc_set_duty(ledc_mode_t_LEDC_LOW_SPEED_MODE, BACKLIGHT_CHANNEL, duty) })?;
        esp!(unsafe { ledc_update_duty(ledc_mode_t_LEDC_LOW_SPEED_MODE, BACKLIGHT_CHANNEL) })
    }
}

fn backlight_init() -> Result<(), EspError> {
    let timer = ledc_timer_config_t {
        speed_mode: ledc_mode_t_LEDC_LOW_SPEED_MODE,
        duty_resolution: ledc_timer_bit_t_LEDC_TIMER_12_BIT,
        timer_num: BACKLIGHT_TIMER,
        freq_hz: 5000,
        clk_cfg: soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK,
        ..Default::default()
    };
    esp!(unsafe { ledc_timer_config(&timer) })?;
    let channel = ledc_channel_config_t {
        gpio_num: BACKLIGHT_GPIO,
        speed_mode: ledc_mode_t_LEDC_LOW_SPEED_MODE,
        channel: BACKLIGHT_CHANNEL,
        intr_type: ledc_intr_type_t_LEDC_INTR_DISABLE,
        timer_sel: BACKLIGHT_TIMER,
        duty: 0,
        hpoint: 0,
        ..Default::default()
    };
    esp!(unsafe { ledc_channel_config(&channel) })
}

fn tx(io: esp_lcd_panel_io_handle_t, cmd: u8, params: &[u8]) -> Result<(), EspError> {
    let data = if params.is_empty() { ptr::null() } else { params.as_ptr() as *const c_void };
    esp!(unsafe { esp_lcd_panel_io_tx_param(io, cmd as i32, data, params.len()) })
}

fn rx(io: esp_lcd_panel_io_handle_t, cmd: u8) -> Result<u8, EspError> {
    let mut value = 0u8;
    esp!(unsafe { esp_lcd_panel_io_rx_param(io, cmd as i32, &mut value as *mut u8 as *mut c_void, 1) })?;
    Ok(value)
}

/// Same sequence as `panel_ili9881c_reset` + `panel_ili9881c_init` in Espressif's driver.
fn ili9881c_init(io: esp_lcd_panel_io_handle_t) -> Result<(), EspError> {
    // Hardware reset was done via the IO expander; this is the driver's software reset.
    tx(io, CMD_SWRESET, &[])?;
    sleep(Duration::from_millis(20));

    // The ID registers are on command page 1.
    tx(io, ILI_CMD_PAGE_SELECT, &[0x98, 0x81, 0x01])?;
    match (rx(io, 0x00), rx(io, 0x01), rx(io, 0x02)) {
        (Ok(a), Ok(b), Ok(c)) => log::info!("ILI9881C id: {a:#04x} {b:#04x} {c:#04x}"),
        _ => log::warn!("ILI9881C id read failed (continuing)"),
    }
    tx(io, ILI_PAD_CONTROL, &[ILI_DSI_2_LANE])?;

    tx(io, ILI_CMD_PAGE_SELECT, &[0x98, 0x81, 0x00])?;
    tx(io, CMD_SLPOUT, &[])?;
    sleep(Duration::from_millis(120));
    tx(io, CMD_MADCTL, &[0x00])?;
    tx(io, CMD_COLMOD, &[COLMOD_RGB565])?;

    for &(cmd, params, delay_ms) in INIT_CMDS {
        tx(io, cmd, params)?;
        if delay_ms > 0 {
            sleep(Duration::from_millis(delay_ms as u64));
        }
    }
    Ok(())
}
