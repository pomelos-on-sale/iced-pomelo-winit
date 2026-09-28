# iced-pomelo-winit

The platform layer for [iced](https://github.com/iced-rs/iced) on [pomelo-os](https://github.com/pomelos-on-sale/pomelo-os):
an ESP32-S3 with a 480×480 AMOLED panel, a touchscreen, and no window system.

Except that it is called **`iced_winit`**. That is not a mistake and not nostalgia:

* a device has no `winit`, so the real `iced_winit` cannot build there;
* iced's `iced` facade depends on `iced_winit` **unconditionally** — it is not behind a feature —
  and `[patch]` matches by *package name*;
* so the only way an app can write `use iced::…` on this board is for something else to answer to
  that name.

This is what answers. Our own code reaches it under a name of its own,
`pomelo-iced-host = { package = "iced_winit", … }`, so the borrowed name appears in one line of a
manifest and never in a `use`.

## What it is

iced's own loop is `iced_winit::run`, which needs winit and a window. Its shape is three phases
owned by `iced_runtime::UserInterface` — build the widget tree, feed it the events, draw it — plus
two that belong to the platform: where the events come from, and where the pixels go. On this board
those two are a touch controller and a panel, so this crate provides them and nothing else: a
[`Board`] a firmware registers, the loop that drives it (`Host`, or `run` when the board is looked
up instead of handed over), and `Tree` for the part of a frame that is neither an app's nor a
renderer's.

Two shapes of program run on it, and the difference is deliberate:

| shape | entry | renderer |
| --- | --- | --- |
| an iced `Program` — what `iced::run(..)` and a desktop app already is | `run`, `Host` | whatever the program names, reached through `<P::Renderer as compositor::Default>::Compositor` |
| this crate's own `App` — no state split, no tasks, no window id | `Application` | `iced-pomelo-gfx`, concretely |

The first is the shape the facade lands in, and it is generic on purpose: a shell that named a
renderer could not accept a program whose renderer is an associated type. The second is what the
apps of this OS are written against, and it is the one that goes away when they move.

## What it needs

* [`iced-pomelo-gfx`](https://github.com/pomelos-on-sale/iced-pomelo-gfx) — the renderer: a frame
  recorded as a flat list of commands, replayed into RGB565, with the compositor that presents it.
* [`pomelo-gfx`](https://github.com/pomelos-on-sale/pomelo-gfx) — the rasteriser underneath.
* a patched iced (`vendor/iced` in `pomelo-os`): the fork adds the `custom` and `pomelo` features
  on `iced_renderer` that this crate selects, and `AtomicUsize` id counters for a target with no
  64-bit atomics.

Both siblings are **git** dependencies: a path would only resolve where the directories happen to
lie next to each other. In `pomelo-os` a `[patch]` points those git sources back at its submodules,
so a build there uses the checkouts that are open.

That third bullet has a consequence worth stating: **this crate does not build on its own.** A bare
checkout takes iced from crates.io, and the build stops in feature resolution —

```text
package `iced_winit` depends on `iced_renderer` with feature `custom` but `iced_renderer`
does not have that feature
```

— because those features are the fork's. The graph that works is an *app's*: it patches iced to the
fork and takes this crate over git. `pomelo-os`'s `vendor/README.md` has the manifest.

## The font

A program written for iced installs no font — on a desktop the operating system has them — so this
crate carries one: a 16 KiB Latin subset of Roboto in `fonts/`, installed when an app or a firmware
has not installed a font of its own. `fonts::install` always wins; `fonts::install_default` only
fills a gap. See `fonts/README.md` for what the subset covers, how it was cut, and its licence
(Apache-2.0, a copy included here).

## Licence

GPL-3.0-only — see [`LICENSE`](LICENSE). The font in `fonts/` is Apache-2.0, and the notice travels
with it.
