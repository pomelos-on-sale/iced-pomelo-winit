//! Which renderer iced hands to an app, asserted at compile time.
//!
//! An app names its renderer once, as `iced::Renderer`, and the element it returns from `view` has
//! to be one this crate can draw. iced reaches that name through `iced_renderer::Renderer`, which
//! is an alias chosen by `#[cfg]` — so "is it ours?" is a property of feature flags spread over
//! three crates rather than of any single line: `iced_renderer`'s `pomelo` (turned on by this
//! crate's `renderer` feature) has to beat `iced_renderer`'s `custom` (turned on unconditionally,
//! because it is what names *a* renderer for the defaults).
//!
//! Nothing in an ordinary build would notice if that wiring came undone — the drawing code goes
//! through `iced_winit::Renderer` either way, and would simply be a renderer nobody's widgets were
//! written against. These functions do notice, because they only compile while the two names still
//! mean the same type.

#![cfg(feature = "renderer")]

use std::marker::PhantomData;

use iced_winit::Renderer;

/// The one difference that matters, stated as a constraint rather than a comment: iced's own name
/// for the renderer, and the crate that implements it, are the same type.
#[test]
fn iced_renderer_names_our_renderer() {
    fn are_the_same_type<T>(_: PhantomData<T>, _: PhantomData<T>) {}

    are_the_same_type(
        PhantomData::<Renderer>,
        PhantomData::<iced_renderer::Renderer>,
    );
    are_the_same_type(
        PhantomData::<iced_renderer::Renderer>,
        PhantomData::<iced_pomelo_gfx::Renderer>,
    );
}
