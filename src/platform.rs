//! Embedded platform host and event loop for the Pomelo OS panel.
//!
//! Directly interfaces with the hardware display (DMA flush) and touch interrupt queue,
//! functioning as the embedded counterpart to `winit::event_loop` and `winit::window`.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use iced_core::theme::Base;
use iced_core::time::Instant;
use iced_core::window;
use iced_core::{Point, Rectangle, Size};
use iced_graphics::compositor::Compositor as _;
use iced_graphics::{compositor, Shell, Viewport};
use iced_program::Program;

use crate::application::Tree;
use crate::conversion;
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
}

/// The application host and event loop: drives touch input, frame scheduling, and presentation.
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
}

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
        })
    }

    pub fn app(&self) -> &ProgramApp<P> {
        &self.program
    }

    /// Queues an event, the way a window would deliver one. For tests, and for a backend whose
    /// inputs outnumber a touchscreen's.
    pub fn push_event(&mut self, event: iced_core::Event) {
        self.tree.push_event(event);
        self.dirty = true;
    }

    /// A finger touched the panel at `point`.
    pub fn touch_down(&mut self, point: Point) {
        self.tree.touch_down(point);
        self.dirty = true;
    }

    /// A finger moved while touching the panel.
    pub fn touch_move(&mut self, point: Point) {
        self.tree.touch_move(point);
        self.dirty = true;
    }

    /// The finger left the panel.
    pub fn touch_up(&mut self) {
        self.tree.touch_up();
        self.dirty = true;
    }

    pub fn update(&mut self, message: P::Message) {
        self.program.update(message);
        self.dirty = true;
    }

    pub fn step(&mut self) -> bool {
        let point = self.board.borrow().touch();

        match (point, self.touching) {
            (Some(point), false) => {
                self.touching = true;
                self.tree.touch_down(point);
            }
            (Some(point), true) => self.tree.touch_move(point),
            (None, true) => {
                self.touching = false;
                self.tree.touch_up();
            }
            (None, false) => {}
        }

        // Drain any pending asynchronous touch events from the hardware queue
        // so the FreeRTOS event queue never saturates and drops critical UP events.
        while let Some(event) = self.board.borrow().wait_touch(0) {
            self.program.broadcast_event(event);
            self.dirty = true;
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

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());

    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}
