//! CAN (TWAI) access for the PwrCAN module. Wraps esp-idf-hal's `CanDriver` (IDF legacy TWAI driver),
//! so the rest of the app only sees [`Can`], [`CanFrame`] and [`BusStatus`].
//!
//! PwrCAN DIP setting: CAN Select CH1 + CH5 -> TX = G6, RX = G7 (see the hardware-reference skill).

pub mod self_test;

use esp_idf_svc::hal::can::config::{Config, Mode as HalMode, Timing};
use esp_idf_svc::hal::can::{CanDriver, Flags, Frame, CAN};
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::gpio::{Gpio6, Gpio7};
use esp_idf_svc::sys::*;

/// CAN bitrates used by OBD-II (ISO 15765-4).
#[allow(dead_code)] // K250 is for vehicles that don't answer at 500k (OBD auto-detect, later)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Bitrate {
    K250,
    K500,
}

impl Bitrate {
    /// Bit timing for the ESP32-P4, whose TWAI controllers run from the 40 MHz XTAL.
    /// esp-idf-hal's `Timing::B500K` assumes an 80 MHz clock and would give 250 kbit/s here.
    /// 1 + tseg1 15 + tseg2 4 = 20 time quanta per bit, sample point 80 %.
    fn timing(self) -> Timing {
        let brp = match self {
            Bitrate::K500 => 4, // 40 MHz / 4 / 20 = 500 kbit/s
            Bitrate::K250 => 8, // 40 MHz / 8 / 20 = 250 kbit/s
        };
        Timing::Custom {
            baudrate_prescaler: brp,
            timing_segment_1: 15,
            timing_segment_2: 4,
            synchronization_jump_width: 3,
            triple_sampling: false,
        }
    }
}

#[allow(dead_code)] // Normal / ListenOnly are for the vehicle (OBD), not the bench self-test
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Normal bus participant (ACKs frames, needs another node to ACK ours).
    Normal,
    /// Self-test: our frames don't need an ACK and, sent with self-reception, are received back
    /// through the transceiver. Works with a single node on the bus.
    SelfTest,
    /// Receive only, never drives the bus (safe for sniffing a vehicle bus).
    ListenOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanFrame {
    pub id: u32,
    pub extended: bool,
    pub data: payload::Data,
}

/// Up to 8 payload bytes, without pulling in a crate.
pub mod payload {
    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Data {
        len: u8,
        bytes: [u8; 8],
    }

    impl Data {
        pub fn new(src: &[u8]) -> Option<Self> {
            (src.len() <= 8).then(|| {
                let mut bytes = [0u8; 8];
                bytes[..src.len()].copy_from_slice(src);
                Self { len: src.len() as u8, bytes }
            })
        }

        pub fn as_slice(&self) -> &[u8] {
            &self.bytes[..self.len as usize]
        }
    }
}

impl CanFrame {
    pub fn new_standard(id: u16, data: &[u8]) -> Option<Self> {
        (id <= 0x7FF).then_some(())?;
        Some(Self { id: id as u32, extended: false, data: payload::Data::new(data)? })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BusState {
    Stopped,
    Running,
    BusOff,
    Recovering,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BusStatus {
    pub state: BusState,
    pub tx_error_counter: u32,
    pub rx_error_counter: u32,
    pub tx_failed: u32,
    pub bus_errors: u32,
    pub rx_missed: u32,
}

pub struct Can {
    driver: CanDriver<'static>,
    mode: Mode,
}

impl Can {
    pub fn new(
        can: CAN<'static>,
        tx: Gpio6<'static>,
        rx: Gpio7<'static>,
        bitrate: Bitrate,
        mode: Mode,
    ) -> Result<Self, EspError> {
        let hal_mode = match mode {
            Mode::Normal => HalMode::Normal,
            Mode::SelfTest => HalMode::NoAck,
            Mode::ListenOnly => HalMode::ListenOnly,
        };
        let config = Config::new().timing(bitrate.timing()).mode(hal_mode);
        let mut driver = CanDriver::new(can, tx, rx, &config)?;
        driver.start()?;
        log::info!("CAN started: {bitrate:?}, {mode:?}, TX=G6 RX=G7");
        Ok(Self { driver, mode })
    }

    /// Queues a frame for transmission (waits up to `timeout_ms` for space in the TX queue).
    /// In self-test mode the frame is sent single-shot with self-reception.
    pub fn send(&self, frame: &CanFrame, timeout_ms: u64) -> Result<(), EspError> {
        // Single-shot: no automatic retransmission, so a broken bus can't flood the controller.
        // (`Default` = empty EnumSet; its type is inferred from `Frame::new`.)
        let mut flags = Default::default();
        flags |= Flags::SingleShot;
        if frame.extended {
            flags |= Flags::Extended;
        }
        if self.mode == Mode::SelfTest {
            flags |= Flags::SelfReception;
        }
        let hal_frame = Frame::new(frame.id, flags, frame.data.as_slice())
            .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_ARG>)?;
        self.driver.transmit(&hal_frame, TickType::new_millis(timeout_ms).ticks())
    }

    /// Waits up to `timeout_ms` for a received frame. `Ok(None)` on timeout.
    pub fn receive(&self, timeout_ms: u64) -> Result<Option<CanFrame>, EspError> {
        match self.driver.receive(TickType::new_millis(timeout_ms).ticks()) {
            Ok(f) => Ok(Some(CanFrame {
                id: f.identifier(),
                extended: f.is_extended(),
                data: payload::Data::new(f.data()).unwrap_or_default(),
            })),
            Err(e) if e.code() == ESP_ERR_TIMEOUT as i32 => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn status(&self) -> Result<BusStatus, EspError> {
        let mut info = twai_status_info_t::default();
        esp!(unsafe { twai_get_status_info(&mut info) })?;
        #[allow(non_upper_case_globals)]
        let state = match info.state {
            twai_state_t_TWAI_STATE_RUNNING => BusState::Running,
            twai_state_t_TWAI_STATE_BUS_OFF => BusState::BusOff,
            twai_state_t_TWAI_STATE_RECOVERING => BusState::Recovering,
            _ => BusState::Stopped,
        };
        Ok(BusStatus {
            state,
            tx_error_counter: info.tx_error_counter,
            rx_error_counter: info.rx_error_counter,
            tx_failed: info.tx_failed_count,
            bus_errors: info.bus_error_count,
            rx_missed: info.rx_missed_count,
        })
    }

    /// Brings the controller back after bus-off: starts recovery, or restarts once recovery finished.
    pub fn recover(&mut self) -> Result<(), EspError> {
        match self.status()?.state {
            BusState::BusOff => esp!(unsafe { twai_initiate_recovery() }),
            BusState::Stopped => self.driver.start(),
            _ => Ok(()),
        }
    }
}
