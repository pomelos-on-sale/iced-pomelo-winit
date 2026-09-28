//! Text, on a device that has no fonts of its own.
//!
//! The important assertion is not "glyphs appeared". On a desktop iced finds a system font, so
//! pixels would appear even with [`fonts::install`] deleted, and the device is where that stops
//! being true. What this checks is the part that actually fails on the device — that the font
//! went into the database *and that iced's default family resolves to it* — and then it checks
//! that the pixels follow. It runs on both renderers, which is the point: the same font system and
//! the same shaping, only the glyph blend underneath is ours.

use std::collections::HashSet;

use iced_core::theme::{Base, Mode};
use iced_core::{Element, Font, Theme};
use iced_widget::{container, text};
use iced_winit::{fonts, App, Application, Renderer};
use pomelo_gfx::rgb888_to_rgb565;

/// What the firmware embeds: a 16 KiB subset, not iced's 441 KiB Fira Sans.
const SUBSET: &[u8] = include_bytes!("../fonts/Roboto-Subset.ttf");

const SIZE: u32 = 256;
const LABEL: &str = "88888";

/// Draws [`LABEL`], either with iced's default font or with a named one.
struct Label(Option<Font>);

impl App for Label {
    type Message = ();
    type Theme = Theme;

    fn theme(&self) -> Theme {
        <Theme as Base>::default(Mode::Dark)
    }

    fn update(&mut self, _: ()) {}

    fn view(&self) -> Element<'_, (), Theme, Renderer> {
        let label = text(LABEL).size(40);

        let label = match self.0 {
            Some(font) => label.font(font),
            None => label,
        };

        container(label).padding(20).into()
    }
}

/// Renders one frame and returns the panel, plus the color every glyph-free pixel has.
fn render(font: Option<Font>) -> (Vec<u16>, u16) {
    let mut app = Application::new(Label(font), SIZE, SIZE).expect("buffers");

    assert_eq!(app.frame().len(), 1, "the first frame draws everything");

    let background = <Theme as Base>::default(Mode::Dark).base().background_color;
    let [r, g, b, _] = background.into_rgba8();

    (app.panel().data().to_vec(), rgb888_to_rgb565(r, g, b))
}

#[test]
fn the_installed_font_becomes_iced_default() {
    let before: HashSet<String> = fonts::families().into_iter().collect();

    fonts::install(SUBSET);

    let added: Vec<String> = fonts::families()
        .into_iter()
        .filter(|family| !before.contains(family))
        .collect();

    assert!(
        !added.is_empty(),
        "install() has to put a face in the database, otherwise the device renders nothing"
    );

    let name: &'static str = Box::leak(added[0].clone().into_boxed_str());

    // The same text, once through the default family and once by name. If `install` had not also
    // made our face the sans-serif family, the default would resolve to something else — iced
    // always embeds an icon font — and these two would not match.
    let (default, background) = render(None);
    let (named, _) = render(Some(Font::with_name(name)));

    assert_eq!(
        default, named,
        "`Font::default()` does not resolve to the installed font ({name})"
    );

    let glyphs = default.iter().filter(|pixel| **pixel != background).count();
    assert!(glyphs > 0, "the label drew nothing at all");
}
