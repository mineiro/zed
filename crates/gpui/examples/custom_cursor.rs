#![cfg_attr(target_family = "wasm", no_main)]

//! Custom image cursors: hover the left panel for a ring drawn by the
//! application, the right one for a named cursor. Where images can't be
//! shown (X11, macOS, Windows, the web), the ring's fallback, a crosshair,
//! shows instead.

use gpui::{
    App, Bounds, Context, CursorImage, CursorStyle, CustomCursor, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

/// A ring with a dot, 24 logical pixels across, drawn at twice that.
fn ring() -> CursorImage {
    let (scale, logical) = (2u32, 24u32);
    let pixels = logical * scale;
    let mut rgba = Vec::with_capacity((pixels * pixels * 4) as usize);
    let centre = pixels as f32 / 2.0;
    for y in 0..pixels {
        for x in 0..pixels {
            let d = ((x as f32 + 0.5 - centre).powi(2) + (y as f32 + 0.5 - centre).powi(2)).sqrt();
            let ring = (d - centre * 0.75).abs() < 2.5 * scale as f32 / 2.0;
            let outline = (d - centre * 0.75).abs() < 4.5 * scale as f32 / 2.0;
            let dot = d < 2.0 * scale as f32;
            let pixel: [u8; 4] = if ring || dot {
                [0x2b, 0x50, 0xd4, 0xff]
            } else if outline {
                [0xff, 0xff, 0xff, 0xff]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    CursorImage {
        rgba,
        width: pixels,
        height: pixels,
        scale,
        hotspot: (logical / 2, logical / 2),
        fallback: CursorStyle::Crosshair,
    }
}

struct Cursors {
    ring: CustomCursor,
}

impl Render for Cursors {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let panel = |label: &'static str| {
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0xffffff))
                .child(label)
        };
        div()
            .size_full()
            .flex()
            .bg(rgb(0x303030))
            .child(
                panel("Custom: a ring")
                    .bg(rgb(0x3a3f52))
                    .cursor(CursorStyle::Custom(self.ring)),
            )
            .child(panel("Named: pointing hand").cursor(CursorStyle::PointingHand))
    }
}

fn run_example() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(320.)), cx);
        let ring = CustomCursor::register(ring());
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Cursors { ring }),
        )
        .unwrap();
        cx.activate(true);
    });
}

#[cfg(not(target_family = "wasm"))]
fn main() {
    run_example();
}

#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    gpui_platform::web_init();
    run_example();
}
