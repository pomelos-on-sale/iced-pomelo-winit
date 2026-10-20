//! Embedded platform host and event loop for the Pomelo OS panel.
//!
//! Directly interfaces with the hardware display (DMA flush) and touch interrupt queue,
//! functioning as the embedded counterpart to `winit::event_loop` and `winit::window`.

#[cfg(feature = "renderer")]
use std::cell::{Cell, RefCell};
#[cfg(feature = "renderer")]
use std::future::Future;
#[cfg(feature = "renderer")]
use std::rc::Rc;
#[cfg(feature = "renderer")]
use std::task::{Context, Poll, Waker};
use std::time::Duration;

#[cfg(feature = "renderer")]
use iced_core::theme::Base;
#[cfg(feature = "renderer")]
use iced_core::time::Instant;
#[cfg(feature = "renderer")]
use iced_core::window;
use iced_core::{Point, Rectangle, Size};
#[cfg(feature = "renderer")]
use iced_graphics::compositor::Compositor as _;
#[cfg(feature = "renderer")]
use iced_graphics::{compositor, Shell, Viewport};
#[cfg(feature = "renderer")]
use iced_program::Program;

#[cfg(feature = "renderer")]
use crate::application::Tree;
use crate::conversion;
#[cfg(feature = "renderer")]
use crate::{Error, ProgramApp};

/// The panel width in physical pixels.
pub const PANEL_WIDTH: u32 = 480;

/// The panel height in physical pixels.
pub const PANEL_HEIGHT: u32 = 480;

/// Physical dimensions of the display panel.
pub const PANEL_SIZE: Size<u32> = Size::new(PANEL_WIDTH, PANEL_HEIGHT);

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalTouchEvType {
    None = 0,
    Down = 1,
    Move = 2,
    Up = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HalTouchEvent {
    pub ev_type: HalTouchEvType,
    pub x: i32,
    pub y: i32,
}

#[cfg(target_os = "espidf")]
extern "C" {
    fn hal_display_draw_bitmap(x1: i32, y1: i32, x2: i32, y2: i32, pixels: *const u16, stride: i32);
    fn hal_display_wait_vsync();
    fn hal_touch_get_point(out_x: *mut i32, out_y: *mut i32) -> bool;
    fn hal_touch_wait_event(out_ev: *mut HalTouchEvent, timeout_ms: u32) -> bool;
    fn hal_display_set_power(on: bool);
    fn hal_display_is_active() -> bool;
    fn hal_display_get_screen_off_timeout_ms() -> u32;
    fn hal_power_is_usb_connected() -> bool;
    fn hal_power_get_stay_awake_on_usb() -> bool;
    fn hal_power_get_keep_screen_on() -> bool;
    fn vTaskDelay(ticks: u32);
}

#[cfg(not(target_os = "espidf"))]
static SIM_SCREEN_OFF_TIMEOUT_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(not(target_os = "espidf"))]
static SIM_STAY_AWAKE_ON_USB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
#[cfg(not(target_os = "espidf"))]
static SIM_KEEP_SCREEN_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(not(target_os = "espidf"))]
#[allow(unused_variables)]
unsafe fn hal_display_draw_bitmap(x1: i32, y1: i32, x2: i32, y2: i32, pixels: *const u16, stride: i32) {}

#[cfg(not(target_os = "espidf"))]
unsafe fn hal_display_wait_vsync() {}

#[cfg(not(target_os = "espidf"))]
#[allow(unused_variables)]
unsafe fn hal_touch_get_point(out_x: *mut i32, out_y: *mut i32) -> bool {
    false
}

#[cfg(not(target_os = "espidf"))]
#[allow(unused_variables)]
unsafe fn hal_touch_wait_event(out_ev: *mut HalTouchEvent, timeout_ms: u32) -> bool {
    false
}

#[cfg(not(target_os = "espidf"))]
#[allow(unused_variables)]
unsafe fn hal_display_set_power(on: bool) {}

#[cfg(not(target_os = "espidf"))]
unsafe fn hal_display_is_active() -> bool {
    true
}

#[cfg(not(target_os = "espidf"))]
unsafe fn hal_display_get_screen_off_timeout_ms() -> u32 {
    SIM_SCREEN_OFF_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(not(target_os = "espidf"))]
unsafe fn hal_power_is_usb_connected() -> bool {
    true
}

#[cfg(not(target_os = "espidf"))]
unsafe fn hal_power_get_stay_awake_on_usb() -> bool {
    SIM_STAY_AWAKE_ON_USB.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(not(target_os = "espidf"))]
unsafe fn hal_power_get_keep_screen_on() -> bool {
    SIM_KEEP_SCREEN_ON.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(not(target_os = "espidf"))]
#[allow(unused_variables, non_snake_case)]
unsafe fn vTaskDelay(ticks: u32) {}

/// Reads the current touch point from the atomic driver cache.
pub fn touch_point() -> Option<Point> {
    let (mut x, mut y) = (0i32, 0i32);
    if unsafe { hal_touch_get_point(&mut x, &mut y) } {
        let clamped_x = x.clamp(0, PANEL_WIDTH as i32 - 1);
        let clamped_y = y.clamp(0, PANEL_HEIGHT as i32 - 1);
        Some(Point::new(clamped_x as f32, clamped_y as f32))
    } else {
        None
    }
}

/// Waits for a touch event from the interrupt-driven FreeRTOS queue.
pub fn wait_touch_event(timeout_ms: u32) -> Option<iced_core::Event> {
    let mut ev = std::mem::MaybeUninit::uninit();
    if unsafe { hal_touch_wait_event(ev.as_mut_ptr(), timeout_ms) } {
        let ev = unsafe { ev.assume_init() };
        conversion::touch_event(ev)
    } else {
        None
    }
}

/// Hands damaged rectangular regions of the frame buffer to the panel via DMA.
pub fn flush_damage(damage: &[Rectangle], pixels: &[u16]) {
    if damage.is_empty() {
        return;
    }

    // Merge multiple disjoint rectangles during swiping into a single continuous bounding box
    // to prevent multi-pass fragmented DMA transfers that cause visual tearing and command stalls.
    let mut union = damage[0];
    for bounds in &damage[1..] {
        union = union.union(bounds);
    }

    // Enforce 2-pixel aligned boundaries for hardware CO5300 QSPI DMA controller
    let x1 = (union.x.floor() as i32).max(0) / 2 * 2;
    let y1 = (union.y.floor() as i32).max(0) / 2 * 2;
    let x2 = (((union.x + union.width).ceil() as i32 + 1) / 2 * 2).min(PANEL_WIDTH as i32);
    let y2 = (((union.y + union.height).ceil() as i32 + 1) / 2 * 2).min(PANEL_HEIGHT as i32);

    if x2 > x1 && y2 > y1 {
        unsafe {
            hal_display_draw_bitmap(x1, y1, x2, y2, pixels.as_ptr(), PANEL_WIDTH as i32);
        }
    }
}

/// Yields task execution to FreeRTOS.
pub fn delay_ticks(ticks: u32) {
    unsafe {
        vTaskDelay(ticks);
    }
}

/// The panel and the finger: the hardware the loop reads and writes.
///
/// This is the embedded counterpart of a window. `winit` *creates* its window, so `iced_winit`
/// never has to be told what to draw on; here the hardware exists before the software, so it is
/// handed over instead — [`Host::new`] takes one, and a test substitutes its own. It is the one
/// seam the two platforms cannot share, and keeping it explicit is what makes the loop testable
/// without a panel.
pub trait Board {
    /// The panel size, in physical pixels.
    fn size(&self) -> (u32, u32);

    /// Where the finger is now, if it is down. Polled once a step.
    fn touch(&self) -> Option<Point> {
        None
    }

    /// Waits for an input event, up to `timeout_ms`. `None` on timeout.
    ///
    /// The default times out at once, which is what a test wants: the loop then runs as fast as
    /// the caller steps it.
    fn wait_touch(&self, _timeout_ms: u32) -> Option<iced_core::Event> {
        None
    }

    /// Hands the damaged rectangles to the panel, with the frame in its pixels.
    fn flush(&mut self, damage: &[Rectangle], pixels: &[u16]);

    /// Nothing was drawn: the panel's chance to yield or sleep.
    ///
    /// The default does nothing. This is where the firmware's loop lets FreeRTOS run, and it is
    /// what an idle screen costs.
    fn idle(&mut self) {}

    /// Turns the display panel power on or off (e.g. AMOLED sleep / wake).
    fn set_display_power(&mut self, _on: bool) {}

    /// Whether the display panel is currently powered on.
    fn is_display_on(&self) -> bool {
        true
    }

    /// Inactivity timeout before the screen automatically turns off.
    /// `None` means never turn off automatically.
    fn screen_off_timeout(&self) -> Option<Duration> {
        None
    }

    /// Whether USB (VBUS) is currently connected.
    fn is_usb_connected(&self) -> bool {
        false
    }

    /// Whether to stay awake when USB is connected (defaults to true).
    fn stay_awake_on_usb(&self) -> bool {
        true
    }

    /// Whether the system is requested to keep the screen on by an active app (defaults to false).
    fn is_keep_screen_on(&self) -> bool {
        false
    }
}

/// The real panel: the CO5300 over the C side's `hal_display_*`, and the touch controller behind
/// `hal_touch_*`. This is the [`Board`] the firmware gets, through [`crate::run`].
pub struct Panel;

impl Board for Panel {
    fn size(&self) -> (u32, u32) {
        (PANEL_WIDTH, PANEL_HEIGHT)
    }

    fn touch(&self) -> Option<Point> {
        touch_point()
    }

    fn wait_touch(&self, timeout_ms: u32) -> Option<iced_core::Event> {
        wait_touch_event(timeout_ms)
    }

    fn flush(&mut self, damage: &[Rectangle], pixels: &[u16]) {
        unsafe {
            hal_display_wait_vsync();
        }

        flush_damage(damage, pixels);
    }

    fn idle(&mut self) {
        // One frame's worth of ticks, so a loop with nothing to do does not spin.
        delay_ticks(1);
    }

    fn set_display_power(&mut self, on: bool) {
        unsafe {
            hal_display_set_power(on);
        }
    }

    fn is_display_on(&self) -> bool {
        unsafe {
            hal_display_is_active()
        }
    }

    fn screen_off_timeout(&self) -> Option<Duration> {
        let ms = unsafe { hal_display_get_screen_off_timeout_ms() };
        if ms == 0 {
            None
        } else {
            Some(Duration::from_millis(ms as u64))
        }
    }

    fn is_usb_connected(&self) -> bool {
        unsafe { hal_power_is_usb_connected() }
    }

    fn stay_awake_on_usb(&self) -> bool {
        unsafe { hal_power_get_stay_awake_on_usb() }
    }

    fn is_keep_screen_on(&self) -> bool {
        unsafe { hal_power_get_keep_screen_on() }
    }
}

/// The display and system power state of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    /// Screen is on and interactive.
    Awake,
    /// Screen is powered off (UI rendering is paused, processes/tasks still running).
    ScreenOff,
    /// Screen was just turned on by a touch gesture; initial touch is swallowed until finger lifts.
    WakingUp,
}

/// The application host and event loop: drives touch input, frame scheduling, and presentation.
#[cfg(feature = "renderer")]
pub struct Host<P>
where
    P: Program,
{
    program: ProgramApp<P>,
    tree: Tree,
    messages: Vec<P::Message>,
    dirty: bool,
    redraw_requested: bool,
    compositor: <P::Renderer as compositor::Default>::Compositor,
    renderer: P::Renderer,
    surface: <<P::Renderer as compositor::Default>::Compositor as compositor::Compositor>::Surface,
    viewport: Viewport,
    flushed: Rc<Cell<bool>>,
    /// The hardware. Shared with the compositor's present callback, which is installed as a
    /// global and outlives any borrow of `self`.
    board: Rc<RefCell<Box<dyn Board>>>,
    touching: bool,
    pending_size: Option<Size>,
    screen_state: ScreenState,
    first_frame_rendered: bool,
    last_activity: Instant,
    screen_off_at: Option<Instant>,
    screen_off_timeout_override: Option<Option<Duration>>,
    stay_awake_on_usb_override: Option<bool>,
}

#[cfg(feature = "renderer")]
impl<P> Host<P>
where
    P: Program + 'static,
{
    /// Takes `program` and the hardware it draws on and reads.
    ///
    /// The board is what `winit` would have created: [`crate::run`] hands over the panel's, and a
    /// test hands over its own.
    pub fn new(program: P, board: Box<dyn Board>) -> Result<Self, Error> {
        crate::fonts::install_default();

        let (width, height) = board.size();
        let board = Rc::new(RefCell::new(board));

        let flushed = Rc::new(Cell::new(false));
        {
            let flushed = Rc::clone(&flushed);
            let board = Rc::clone(&board);
            iced_pomelo_gfx::panel::set(move |pixels, damage| {
                flushed.set(true);
                board.borrow_mut().flush(damage, pixels);
            });
        }

        let settings = program.settings();
        let graphics: iced_graphics::Settings = settings.clone().into();

        let mut compositor = block_on(
            <<P::Renderer as compositor::Default>::Compositor as compositor::Compositor>::new(
                graphics,
                iced_pomelo_gfx::Panel,
                iced_pomelo_gfx::Panel,
                Shell::headless(),
            ),
        )
        .map_err(Error::GraphicsCreationFailed)?;

        for font in settings.fonts {
            compositor.load_font(font);
        }

        let renderer = compositor.create_renderer();
        let surface = compositor.create_surface(iced_pomelo_gfx::Panel, width, height);

        let now = Instant::now();
        Ok(Self {
            program: ProgramApp::new(program),
            tree: Tree::new(),
            messages: Vec::new(),
            dirty: true,
            redraw_requested: false,
            compositor,
            renderer,
            surface,
            viewport: Viewport::with_physical_size(Size::new(width, height), 1.0),
            flushed,
            board,
            touching: false,
            pending_size: Some(Size::new(width as f32, height as f32)),
            screen_state: ScreenState::Awake,
            first_frame_rendered: false,
            last_activity: now,
            screen_off_at: None,
            screen_off_timeout_override: None,
            stay_awake_on_usb_override: None,
        })
    }

    pub fn app(&self) -> &ProgramApp<P> {
        &self.program
    }

    /// Returns the current screen power state.
    pub fn screen_state(&self) -> ScreenState {
        self.screen_state
    }

    /// Returns the active inactivity timeout before the screen automatically turns off.
    pub fn screen_off_timeout(&self) -> Option<Duration> {
        self.screen_off_timeout_override
            .unwrap_or_else(|| self.board.borrow().screen_off_timeout())
    }

    /// Sets the inactivity timeout before the screen automatically turns off.
    /// `None` means never automatically turn off.
    pub fn set_screen_off_timeout(&mut self, timeout: Option<Duration>) {
        self.screen_off_timeout_override = Some(timeout);
    }

    /// Returns whether the system stays awake while connected to USB.
    pub fn stay_awake_on_usb(&self) -> bool {
        self.stay_awake_on_usb_override
            .unwrap_or_else(|| self.board.borrow().stay_awake_on_usb())
    }

    /// Sets whether the system stays awake while connected to USB.
    pub fn set_stay_awake_on_usb(&mut self, stay_awake: bool) {
        self.stay_awake_on_usb_override = Some(stay_awake);
    }

    /// Explicitly wake up the display.
    pub fn wake_up(&mut self) {
        if self.screen_state != ScreenState::Awake {
            self.board.borrow_mut().set_display_power(true);
            self.screen_state = ScreenState::Awake;
            self.screen_off_at = None;
            self.last_activity = Instant::now();
            self.dirty = true;
        }
    }

    /// Explicitly turn the screen off (processes/tasks keep running).
    pub fn screen_off(&mut self) {
        if self.screen_state == ScreenState::Awake {
            if self.touching {
                self.touching = false;
                self.tree.touch_up();
            }
            self.board.borrow_mut().set_display_power(false);
            self.screen_state = ScreenState::ScreenOff;
            self.screen_off_at = Some(Instant::now());
        }
    }

    /// Queues an event, the way a window would deliver one. For tests, and for a backend whose
    /// inputs outnumber a touchscreen's.
    pub fn push_event(&mut self, event: iced_core::Event) {
        self.last_activity = Instant::now();
        self.tree.push_event(event);
        self.dirty = true;
    }

    /// A finger touched the panel at `point`.
    pub fn touch_down(&mut self, point: Point) {
        self.last_activity = Instant::now();
        self.tree.touch_down(point);
        self.dirty = true;
    }

    /// A finger moved while touching the panel.
    pub fn touch_move(&mut self, point: Point) {
        self.last_activity = Instant::now();
        self.tree.touch_move(point);
        self.dirty = true;
    }

    /// The finger left the panel.
    pub fn touch_up(&mut self) {
        self.last_activity = Instant::now();
        self.tree.touch_up();
        self.dirty = true;
    }

    pub fn update(&mut self, message: P::Message) {
        self.last_activity = Instant::now();
        self.program.update(message);
        self.dirty = true;
    }

    pub fn step(&mut self) -> bool {
        // 1. Sync screen state if hardware display was toggled externally (e.g. by physical button).
        // Only sync display-off after the initial frame has rendered, since displays start blanked.
        let display_is_on = self.board.borrow().is_display_on();
        if self.first_frame_rendered {
            if !display_is_on && self.screen_state == ScreenState::Awake {
                if self.touching {
                    self.touching = false;
                    self.tree.touch_up();
                }
                self.screen_state = ScreenState::ScreenOff;
                self.screen_off_at = Some(Instant::now());
            }
        }
        if display_is_on && (self.screen_state == ScreenState::ScreenOff) {
            self.screen_state = ScreenState::Awake;
            self.screen_off_at = None;
            self.last_activity = Instant::now();
            self.dirty = true;
        }

        // 2. Check inactivity timeout when Awake -> ScreenOff
        let now = Instant::now();
        if self.screen_state == ScreenState::Awake && !self.touching {
            let stay_awake = (self.stay_awake_on_usb() && self.board.borrow().is_usb_connected())
                || self.board.borrow().is_keep_screen_on();
            if stay_awake {
                // While kept awake by policy (e.g. alarm ringing, stay awake on USB),
                // continuously refresh last_activity so that once the keep-awake policy ends,
                // the full auto-screen-off timeout window begins anew rather than instantly powering off.
                self.last_activity = now;
            } else if let Some(timeout) = self.screen_off_timeout() {
                if now.duration_since(self.last_activity) >= timeout {
                    self.screen_state = ScreenState::ScreenOff;
                    self.screen_off_at = Some(now);
                    self.board.borrow_mut().set_display_power(false);
                }
            }
        }

        // 3. Handle touch & event stream according to screen state
        let point = self.board.borrow().touch();

        match self.screen_state {
            ScreenState::ScreenOff => {
                // Drain and discard any buffered touch interrupt events while screen is off
                while self.board.borrow().wait_touch(0).is_some() {}

                if let Some(_pt) = point {
                    // Touch on ScreenOff panel: WAKE UP & SWALLOW
                    println!("[display] ScreenOff woken up by touch point");
                    self.board.borrow_mut().set_display_power(true);
                    self.screen_state = ScreenState::WakingUp;
                    self.touching = true;
                    self.screen_off_at = None;
                    self.last_activity = Instant::now();
                    self.dirty = true;
                    // Do NOT pass this initial touch to tree or program!
                } else {
                    // Poll background subscriptions and tasks even while screen is off,
                    // without rendering/drawing frames to save CPU and display power.
                    for message in self.program.poll() {
                        self.program.update(message);
                        self.dirty = true;
                    }

                    // Check if program update explicitly woke up the screen
                    if self.board.borrow().is_display_on() {
                        self.screen_state = ScreenState::Awake;
                        self.screen_off_at = None;
                        self.last_activity = Instant::now();
                        self.dirty = true;
                        self.board.borrow_mut().idle();
                        return false;
                    } else {
                        // Remain in ScreenOff: wait for touch or short timeout
                        let event = self.board.borrow().wait_touch(50);
                        let is_touch_down = match event {
                            Some(iced_core::Event::Touch(iced_core::touch::Event::FingerPressed { .. })) => true,
                            _ => false,
                        };
                        if is_touch_down || self.board.borrow().touch().is_some() {
                            println!("[display] ScreenOff woken up by touch tap");
                            self.board.borrow_mut().set_display_power(true);
                            self.screen_state = ScreenState::WakingUp;
                            self.touching = true;
                            self.screen_off_at = None;
                            self.last_activity = Instant::now();
                            self.dirty = true;
                        } else {
                            self.board.borrow_mut().idle();
                        }
                        return false;
                    }
                }
            }
            ScreenState::WakingUp => {
                // Drain and discard any buffered touch interrupt events during wake-up gesture
                while self.board.borrow().wait_touch(0).is_some() {}

                if point.is_some() {
                    // Finger is still held down from the wake gesture: continue swallowing!
                    self.last_activity = Instant::now();
                } else {
                    // Finger was lifted: wake gesture completed! Now fully awake.
                    self.screen_state = ScreenState::Awake;
                    self.touching = false;
                    self.screen_off_at = None;
                    self.last_activity = Instant::now();
                    self.dirty = true;
                }
            }
            ScreenState::Awake => {
                match (point, self.touching) {
                    (Some(point), false) => {
                        self.touching = true;
                        self.last_activity = Instant::now();
                        self.tree.touch_down(point);
                    }
                    (Some(point), true) => {
                        self.last_activity = Instant::now();
                        self.tree.touch_move(point);
                    }
                    (None, true) => {
                        self.touching = false;
                        self.last_activity = Instant::now();
                        self.tree.touch_up();
                    }
                    (None, false) => {}
                }

                // Drain any pending asynchronous touch events from the hardware queue
                while let Some(event) = self.board.borrow().wait_touch(0) {
                    self.last_activity = Instant::now();
                    self.program.broadcast_event(event);
                    self.dirty = true;
                }
            }
        }

        self.flushed.set(false);
        self.frame();
        let painted = self.flushed.get();
        if !self.first_frame_rendered {
            self.first_frame_rendered = true;
            self.screen_state = ScreenState::Awake;
            self.screen_off_at = None;
            self.last_activity = Instant::now();
            self.board.borrow_mut().set_display_power(true);
        }

        if !painted {
            let timeout = match self.tree.next_redraw() {
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        0
                    } else {
                        let ms = (at - now).as_millis();
                        (ms.min(500) as u32).max(1)
                    }
                }
                None => 16,
            };

            let event = self.board.borrow().wait_touch(timeout);

            if let Some(event) = event {
                self.last_activity = Instant::now();
                self.program.broadcast_event(event);
                self.dirty = true;
            } else {
                // Nothing to draw and no input: the board's chance to sleep. This is what an
                // idle screen costs, and the only place the loop yields.
                self.board.borrow_mut().idle();
            }
        }

        painted
    }

    pub fn run(&mut self) -> Result<(), Error> {
        loop {
            self.step();
        }
    }

    fn frame(&mut self) {
        if let Some(size) = self.pending_size.take() {
            self.program.broadcast(window::Event::Resized(size));
        }

        self.program
            .broadcast(window::Event::RedrawRequested(Instant::now()));

        self.messages.extend(self.program.poll());

        // What a task asked the tree to do. A widget operation needs the `UserInterface`, which
        // `Tree::draw` builds, so it is handed to the tree rather than applied here; and a frame
        // with one pending is not one to skip, or the operation would wait for an input that may
        // never come.
        let operations = self.program.take_operations();
        self.tree.queue_operations(operations);

        let scheduled_redraw_due = self
            .tree
            .next_redraw()
            .is_some_and(|at| Instant::now() >= at);

        if !self.dirty
            && !self.redraw_requested
            && !scheduled_redraw_due
            && self.tree.is_idle()
            && self.messages.is_empty()
            && !self.tree.has_operations()
        {
            return;
        }

        let theme = self.program.theme();
        let base = theme.base();
        let bounds = self.viewport.logical_size();

        let t0 = Instant::now();
        self.redraw_requested = self.tree.draw(
            &mut self.renderer,
            self.program.view(),
            &mut self.messages,
            &theme,
            bounds,
            base.text_color,
        );

        let _ = self.compositor.present(
            &mut self.renderer,
            &mut self.surface,
            &self.viewport,
            base.background_color,
            || {},
        );
        let render_ms = t0.elapsed().as_millis();
        println!("[perf] render took {}ms", render_ms);

        self.dirty = !self.messages.is_empty();

        for message in self.messages.drain(..) {
            self.program.update(message);
        }

        for message in self.program.poll() {
            self.dirty = true;
            self.program.update(message);
        }
    }
}

#[cfg(feature = "renderer")]
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());

    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

#[cfg(all(test, feature = "renderer"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use iced_core::{Element, Length, Settings};
    use iced_futures::backend::null;
    use iced_runtime::Task;
    use iced_widget::{button, text};

    struct TestBoard {
        touch_point: Rc<Cell<Option<Point>>>,
        display_powered: Rc<Cell<bool>>,
    }

    impl Board for TestBoard {
        fn size(&self) -> (u32, u32) {
            (480, 480)
        }

        fn touch(&self) -> Option<Point> {
            self.touch_point.get()
        }

        fn flush(&mut self, _damage: &[Rectangle], _pixels: &[u16]) {}

        fn set_display_power(&mut self, on: bool) {
            self.display_powered.set(on);
        }

        fn is_display_on(&self) -> bool {
            self.display_powered.get()
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TestMsg {
        Clicked,
        Wake,
    }

    struct TestApp {
        clicked: Arc<AtomicBool>,
        display_powered: Option<Rc<Cell<bool>>>,
    }

    impl Program for TestApp {
        type State = ();
        type Message = TestMsg;
        type Theme = iced_core::Theme;
        type Renderer = crate::Renderer;
        type Executor = null::Executor;

        fn name() -> &'static str {
            "test_app"
        }

        fn settings(&self) -> Settings {
            Settings::default()
        }

        fn window(&self) -> Option<iced_core::window::Settings> {
            None
        }

        fn boot(&self) -> (Self::State, Task<Self::Message>) {
            ((), Task::none())
        }

        fn update(&self, _state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
            match message {
                TestMsg::Clicked => self.clicked.store(true, Ordering::SeqCst),
                TestMsg::Wake => {
                    if let Some(dp) = &self.display_powered {
                        dp.set(true);
                    }
                }
            }
            Task::none()
        }

        fn view<'a>(
            &self,
            _state: &'a Self::State,
            _window: iced_core::window::Id,
        ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
            button(text("Click Me"))
                .on_press(TestMsg::Clicked)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }

    #[test]
    fn host_auto_screen_off_and_wake_up_touch_swallow() {
        let touch_point = Rc::new(Cell::new(None));
        let display_powered = Rc::new(Cell::new(true));
        let board = Box::new(TestBoard {
            touch_point: Rc::clone(&touch_point),
            display_powered: Rc::clone(&display_powered),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: None,
        };
        let mut host = Host::new(app, board).unwrap();

        // 1. Initial state: Awake, display ON
        assert_eq!(host.screen_state(), ScreenState::Awake);
        assert!(display_powered.get());

        // Initial frame draw
        host.step();
        assert!(!clicked.load(Ordering::SeqCst));

        // 2. Set small timeout for test
        host.set_screen_off_timeout(Some(Duration::from_millis(10)));
        std::thread::sleep(Duration::from_millis(15));

        // Step should transition to ScreenOff and power off display
        host.step();
        assert_eq!(host.screen_state(), ScreenState::ScreenOff);
        assert!(!display_powered.get(), "display should have been powered off");

        // 3. User touches screen to wake up (finger down at center 240, 240)
        touch_point.set(Some(Point::new(240.0, 240.0)));
        host.step();

        // Screen powers back ON and enters WakingUp state
        assert_eq!(host.screen_state(), ScreenState::WakingUp);
        assert!(display_powered.get(), "display should have powered back on");
        // Crucial: wake-up touch must NOT click the button!
        assert!(!clicked.load(Ordering::SeqCst), "wake-up touch must be swallowed!");

        // Finger moves slightly (still held down)
        touch_point.set(Some(Point::new(241.0, 241.0)));
        host.step();
        assert_eq!(host.screen_state(), ScreenState::WakingUp);
        assert!(
            !clicked.load(Ordering::SeqCst),
            "move during wake gesture must still be swallowed"
        );

        // Finger is lifted
        touch_point.set(None);
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake);
        assert!(!clicked.load(Ordering::SeqCst), "lift must not click button");

        // 4. Now that screen is awake, the next touch normally clicks the button!
        touch_point.set(Some(Point::new(240.0, 240.0)));
        host.step();
        touch_point.set(None);
        host.step();
        assert!(
            clicked.load(Ordering::SeqCst),
            "touch while awake should click the button"
        );
    }

    #[test]
    fn host_external_button_wake() {
        let touch_point = Rc::new(Cell::new(None));
        let display_powered = Rc::new(Cell::new(true));
        let board = Box::new(TestBoard {
            touch_point: Rc::clone(&touch_point),
            display_powered: Rc::clone(&display_powered),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: None,
        };
        let mut host = Host::new(app, board).unwrap();

        // Put screen to screen off
        host.screen_off();
        assert_eq!(host.screen_state(), ScreenState::ScreenOff);
        assert!(!display_powered.get());

        // External physical button wakes display (board.is_display_on becomes true)
        display_powered.set(true);

        // Host steps and notices hardware display is ON
        host.step();
        assert_eq!(
            host.screen_state(),
            ScreenState::Awake,
            "external display power on wakes host"
        );
    }

    #[test]
    fn host_does_not_turn_off_screen_at_boot_when_display_starts_blanked() {
        let touch_point = Rc::new(Cell::new(None));
        // On real hardware, panel starts blanked (display_powered = false) until first frame!
        let display_powered = Rc::new(Cell::new(false));
        let board = Box::new(TestBoard {
            touch_point: Rc::clone(&touch_point),
            display_powered: Rc::clone(&display_powered),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: None,
        };
        let mut host = Host::new(app, board).unwrap();

        // 1. Initial state must be Awake
        assert_eq!(host.screen_state(), ScreenState::Awake);

        // 2. Initial step must render and STAY Awake (not immediately go to screen off!)
        host.step();
        assert_eq!(
            host.screen_state(),
            ScreenState::Awake,
            "host must remain Awake on boot to render first frame even if display starts blanked"
        );
        assert!(display_powered.get(), "first frame must power on the display");
    }

    #[test]
    fn host_does_not_turn_off_screen_when_screen_off_timeout_is_none() {
        let touch_point = Rc::new(Cell::new(None));
        let display_powered = Rc::new(Cell::new(true));
        let board = Box::new(TestBoard {
            touch_point: Rc::clone(&touch_point),
            display_powered: Rc::clone(&display_powered),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: None,
        };
        let mut host = Host::new(app, board).unwrap();

        // Initial frame draw
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake);

        // Timeout is None by default
        assert_eq!(host.screen_off_timeout(), None);
        std::thread::sleep(Duration::from_millis(20));

        // Step should remain Awake
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake);
        assert!(display_powered.get());
    }

    #[test]
    fn host_background_poll_and_wake_while_screen_off() {
        let touch_point = Rc::new(Cell::new(None));
        let display_powered = Rc::new(Cell::new(true));
        let board = Box::new(TestBoard {
            touch_point: Rc::clone(&touch_point),
            display_powered: Rc::clone(&display_powered),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: Some(Rc::clone(&display_powered)),
        };
        let mut host = Host::new(app, board).unwrap();

        // 1. Initial draw
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake);

        // 2. Put host to screen off
        host.screen_off();
        assert_eq!(host.screen_state(), ScreenState::ScreenOff);
        assert!(!display_powered.get());

        // 3. While screen is off, an async event or background update sets display power
        host.program.update(TestMsg::Wake);
        // Step processes background state and notices display is now ON
        host.step();

        assert_eq!(host.screen_state(), ScreenState::Awake);
        assert!(display_powered.get());
    }

    #[test]
    fn host_stay_awake_on_usb() {
        struct UsbBoard {
            display_powered: Rc<Cell<bool>>,
            usb_connected: Rc<Cell<bool>>,
        }
        impl Board for UsbBoard {
            fn size(&self) -> (u32, u32) { (480, 480) }
            fn flush(&mut self, _damage: &[Rectangle], _pixels: &[u16]) {}
            fn set_display_power(&mut self, on: bool) { self.display_powered.set(on); }
            fn is_display_on(&self) -> bool { self.display_powered.get() }
            fn is_usb_connected(&self) -> bool { self.usb_connected.get() }
        }

        let display_powered = Rc::new(Cell::new(true));
        let usb_connected = Rc::new(Cell::new(true));
        let board = Box::new(UsbBoard {
            display_powered: Rc::clone(&display_powered),
            usb_connected: Rc::clone(&usb_connected),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: None,
        };
        let mut host = Host::new(app, board).unwrap();
        host.set_screen_off_timeout(Some(Duration::from_millis(10)));
        host.set_stay_awake_on_usb(true);

        // 1. Inactivity reaches screen off timeout, but USB is connected: MUST REMAIN Awake!
        std::thread::sleep(Duration::from_millis(15));
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake, "stay_awake_on_usb must keep state in Awake");
        assert!(display_powered.get());

        // 2. Disconnect USB: on next step, inactivity triggers transition to ScreenOff
        usb_connected.set(false);
        std::thread::sleep(Duration::from_millis(15));
        host.step();
        assert_eq!(host.screen_state(), ScreenState::ScreenOff, "disconnecting USB allows transitioning to ScreenOff");
        assert!(!display_powered.get());
    }

    #[test]
    fn host_keep_screen_on_override() {
        struct AwakeBoard {
            display_powered: Rc<Cell<bool>>,
            keep_screen_on: Rc<Cell<bool>>,
        }
        impl Board for AwakeBoard {
            fn size(&self) -> (u32, u32) { (480, 480) }
            fn flush(&mut self, _damage: &[Rectangle], _pixels: &[u16]) {}
            fn set_display_power(&mut self, on: bool) { self.display_powered.set(on); }
            fn is_display_on(&self) -> bool { self.display_powered.get() }
            fn is_keep_screen_on(&self) -> bool { self.keep_screen_on.get() }
        }

        let display_powered = Rc::new(Cell::new(true));
        let keep_screen_on = Rc::new(Cell::new(true));
        let board = Box::new(AwakeBoard {
            display_powered: Rc::clone(&display_powered),
            keep_screen_on: Rc::clone(&keep_screen_on),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
            display_powered: None,
        };
        let mut host = Host::new(app, board).unwrap();
        host.set_screen_off_timeout(Some(Duration::from_millis(10)));
        host.set_stay_awake_on_usb(false);

        // 1. Inactivity reaches timeout, but keep_screen_on is true -> MUST stay Awake
        std::thread::sleep(Duration::from_millis(15));
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake, "keep_screen_on must keep state in Awake");
        assert!(display_powered.get());

        // 2. Clear keep_screen_on -> immediately after clearing, screen must STILL be Awake because timeout window reset
        keep_screen_on.set(false);
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Awake, "clearing keep_screen_on resets timeout window, remaining Awake initially");
        assert!(display_powered.get());

        // 3. Inactivity reaches timeout after clearing keep_screen_on -> transitions to ScreenOff
        std::thread::sleep(Duration::from_millis(15));
        host.step();
        assert_eq!(host.screen_state(), ScreenState::ScreenOff, "clearing keep_screen_on allows ScreenOff after timeout");
        assert!(!display_powered.get());
    }
}

