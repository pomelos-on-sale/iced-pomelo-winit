//! Convert platform hardware events into [`iced_core`] types.
//!
//! Mirrors upstream [`iced_winit::conversion`] for embedded platform input.

use iced_core::touch;
use iced_core::{Event, Point};

use crate::platform::{HalTouchEvent, HalTouchEvType, PANEL_HEIGHT, PANEL_WIDTH};

/// Converts a platform touch event into an [`iced_core::Event::Touch`].
pub fn touch_event(ev: HalTouchEvent) -> Option<Event> {
    let position = Point::new(
        ev.x.clamp(0, PANEL_WIDTH as i32 - 1) as f32,
        ev.y.clamp(0, PANEL_HEIGHT as i32 - 1) as f32,
    );

    let finger_ev = match ev.ev_type {
        HalTouchEvType::Down => touch::Event::FingerPressed {
            id: touch::Finger(0),
            position,
        },
        HalTouchEvType::Move => touch::Event::FingerMoved {
            id: touch::Finger(0),
            position,
        },
        HalTouchEvType::Up => touch::Event::FingerLifted {
            id: touch::Finger(0),
            position,
        },
        HalTouchEvType::None => return None,
    };

    Some(Event::Touch(finger_ev))
}
