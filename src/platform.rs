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
#[cfg(feature = "renderer")]
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
    fn vTaskDelay(ticks: u32);
}

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
}

/// The display and touch interaction power state of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    /// Screen is on and interactive.
    Awake,
    /// Screen is powered off (sleeping).
    Sleeping,
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
    last_activity: Instant,
    sleep_timeout: Duration,
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
            last_activity: now,
            sleep_timeout: Duration::from_secs(30),
        })
    }

    pub fn app(&self) -> &ProgramApp<P> {
        &self.program
    }

    /// Returns the current screen power state.
    pub fn screen_state(&self) -> ScreenState {
        self.screen_state
    }

    /// Sets the inactivity timeout before the screen automatically sleeps.
    pub fn set_sleep_timeout(&mut self, timeout: Duration) {
        self.sleep_timeout = timeout;
    }

    /// Explicitly wake up the display.
    pub fn wake_up(&mut self) {
        if self.screen_state != ScreenState::Awake {
            self.board.borrow_mut().set_display_power(true);
            self.screen_state = ScreenState::Awake;
            self.last_activity = Instant::now();
            self.dirty = true;
        }
    }

    /// Explicitly put the screen to sleep.
    pub fn sleep(&mut self) {
        if self.screen_state == ScreenState::Awake {
            if self.touching {
                self.touching = false;
                self.tree.touch_up();
            }
            self.board.borrow_mut().set_display_power(false);
            self.screen_state = ScreenState::Sleeping;
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
        // 1. Sync screen state if hardware display was toggled externally (e.g. by physical button)
        let display_is_on = self.board.borrow().is_display_on();
        if !display_is_on && self.screen_state == ScreenState::Awake {
            if self.touching {
                self.touching = false;
                self.tree.touch_up();
            }
            self.screen_state = ScreenState::Sleeping;
        } else if display_is_on && self.screen_state == ScreenState::Sleeping {
            self.screen_state = ScreenState::Awake;
            self.last_activity = Instant::now();
            self.dirty = true;
        }

        // 2. Check inactivity timeout when Awake
        let now = Instant::now();
        if self.screen_state == ScreenState::Awake && !self.touching {
            if now.duration_since(self.last_activity) >= self.sleep_timeout {
                self.screen_state = ScreenState::Sleeping;
                self.board.borrow_mut().set_display_power(false);
            }
        }

        // 3. Handle touch & event stream according to screen state
        let point = self.board.borrow().touch();

        match self.screen_state {
            ScreenState::Sleeping => {
                // Drain and discard any buffered touch interrupt events while asleep
                while self.board.borrow().wait_touch(0).is_some() {}

                if let Some(_pt) = point {
                    // First touch on sleeping panel: WAKE UP & SWALLOW
                    self.board.borrow_mut().set_display_power(true);
                    self.screen_state = ScreenState::WakingUp;
                    self.touching = true;
                    self.last_activity = Instant::now();
                    self.dirty = true;
                    // Do NOT pass this initial touch to tree or program!
                } else {
                    // Remain asleep: skip frame presentation to save power/CPU
                    self.board.borrow_mut().idle();
                    return false;
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
        if render_ms >= 16 {
            println!("[perf] frame render took {}ms (>16ms target)", render_ms);
        }

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
    }

    struct TestApp {
        clicked: Arc<AtomicBool>,
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
            if message == TestMsg::Clicked {
                self.clicked.store(true, Ordering::SeqCst);
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
    fn host_auto_sleep_and_wake_up_touch_swallow() {
        let touch_point = Rc::new(Cell::new(None));
        let display_powered = Rc::new(Cell::new(true));
        let board = Box::new(TestBoard {
            touch_point: Rc::clone(&touch_point),
            display_powered: Rc::clone(&display_powered),
        });

        let clicked = Arc::new(AtomicBool::new(false));
        let app = TestApp {
            clicked: Arc::clone(&clicked),
        };
        let mut host = Host::new(app, board).unwrap();

        // 1. Initial state: Awake, display ON
        assert_eq!(host.screen_state(), ScreenState::Awake);
        assert!(display_powered.get());

        // Initial frame draw
        host.step();
        assert!(!clicked.load(Ordering::SeqCst));

        // 2. Set small timeout for test
        host.set_sleep_timeout(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(15));

        // Step should transition to Sleeping and power off display
        host.step();
        assert_eq!(host.screen_state(), ScreenState::Sleeping);
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
        };
        let mut host = Host::new(app, board).unwrap();

        // Put screen to sleep
        host.sleep();
        assert_eq!(host.screen_state(), ScreenState::Sleeping);
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
}
