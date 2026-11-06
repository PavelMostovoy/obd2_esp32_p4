//! Slint integration: platform glue for the software renderer, touch input, and the UI loop.
//! Hardware access goes through `crate::board` only.

use std::rc::Rc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, RenderingRotation, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError};

use crate::board::{Board, TouchPoint, TouchSample, PANEL_HEIGHT, PANEL_WIDTH};

slint::include_modules!();

/// Landscape UI on the portrait-native panel.
const UI_WIDTH: u32 = PANEL_HEIGHT as u32;
const UI_HEIGHT: u32 = PANEL_WIDTH as u32;
const ROTATION: RenderingRotation = RenderingRotation::Rotate90;
const LOOP_PERIOD: Duration = Duration::from_millis(10);

struct EspPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for EspPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn debug_log(&self, arguments: core::fmt::Arguments) {
        log::info!("slint: {arguments}");
    }
}

/// Maps a touch point (panel-native portrait coordinates) to logical landscape UI coordinates.
/// Inverse of Slint's `Rotate90`: physical (px, py) = (PANEL_WIDTH - 1 - y, x).
fn touch_to_ui(p: TouchPoint) -> LogicalPosition {
    let x = p.y as f32;
    let y = (PANEL_WIDTH as f32 - 1.0) - p.x as f32;
    LogicalPosition::new(x, y)
}

pub fn run(mut board: Board) -> ! {
    // Two frame buffers alternate, so Slint redraws what changed in either of the last two frames.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
    window.set_size(PhysicalSize::new(UI_WIDTH, UI_HEIGHT));
    slint::platform::set_platform(Box::new(EspPlatform { window: window.clone(), start: Instant::now() }))
        .expect("Slint platform already set");

    let app = MainWindow::new().expect("create MainWindow");
    let weak = app.as_weak();
    app.on_increment(move || {
        if let Some(app) = weak.upgrade() {
            app.set_value(app.get_value() + 1);
        }
    });
    let weak = app.as_weak();
    app.on_decrement(move || {
        if let Some(app) = weak.upgrade() {
            app.set_value(app.get_value() - 1);
        }
    });
    app.show().expect("show MainWindow");

    let mut last_touch: Option<LogicalPosition> = None;
    // Rows rendered in the previous frame. In SwappedBuffers mode the back buffer also gets those
    // rows redrawn, so they must be written back from the cache too.
    let mut prev_rows: Option<(usize, usize)> = None;
    let mut touch_errors: u32 = 0;

    loop {
        slint::platform::update_timers_and_animations();

        match board.touch.poll() {
            Ok(TouchSample::NoNewData) => {}
            Ok(TouchSample::Pressed(p)) => {
                let pos = touch_to_ui(p);
                let event = if last_touch.is_some() {
                    WindowEvent::PointerMoved { position: pos }
                } else {
                    log::info!("touch down raw=({}, {}) ui=({:.0}, {:.0})", p.x, p.y, pos.x, pos.y);
                    WindowEvent::PointerPressed { position: pos, button: PointerEventButton::Left }
                };
                window.dispatch_event(event);
                last_touch = Some(pos);
                app.set_touch_down(true);
                app.set_touch_x(pos.x);
                app.set_touch_y(pos.y);
            }
            Ok(TouchSample::Released) => {
                if let Some(pos) = last_touch.take() {
                    log::info!("touch up ui=({:.0}, {:.0})", pos.x, pos.y);
                    window.dispatch_event(WindowEvent::PointerReleased {
                        position: pos,
                        button: PointerEventButton::Left,
                    });
                    window.dispatch_event(WindowEvent::PointerExited);
                    app.set_touch_down(false);
                }
            }
            Err(e) => {
                touch_errors += 1;
                if touch_errors.is_power_of_two() {
                    log::warn!("touch read failed ({touch_errors}x): {e}");
                }
            }
        }

        window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(ROTATION);
            let fb = board.display.back_buffer();
            // SAFETY: Rgb565Pixel is #[repr(transparent)] over u16.
            let pixels: &mut [Rgb565Pixel] =
                unsafe { core::slice::from_raw_parts_mut(fb.as_mut_ptr().cast(), fb.len()) };
            let region = renderer.render(pixels, PANEL_WIDTH);
            // The region is in buffer (physical, rotated) coordinates.
            let origin = region.bounding_box_origin();
            let size = region.bounding_box_size();
            let rows = (size.height > 0).then(|| {
                let y0 = origin.y.max(0) as usize;
                (y0, (y0 + size.height as usize).min(PANEL_HEIGHT))
            });
            let flush = match (rows, prev_rows) {
                (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
                (r, p) => r.or(p),
            };
            prev_rows = rows;
            if let Some((y0, y1)) = flush {
                if let Err(e) = board.display.present(y0, y1) {
                    log::warn!("display present failed: {e}");
                }
            }
        });

        sleep(LOOP_PERIOD);
    }
}
