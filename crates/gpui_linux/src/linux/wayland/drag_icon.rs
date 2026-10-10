//! The icon an outbound file drag carries: a page, or a small stack of pages
//! for several files, drawn into a shared-memory buffer. Without one the
//! compositor shows nothing under the pointer once the drag leaves the window.

use std::{
    fs::File,
    io::Write as _,
    os::fd::{AsFd as _, FromRawFd as _, OwnedFd},
};

use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer, wl_shm, wl_surface},
};

use super::client::{Globals, WaylandClientStatePtr};

/// One page, in logical pixels.
const PAGE_WIDTH: f32 = 26.0;
const PAGE_HEIGHT: f32 = 32.0;
const FOLD: f32 = 8.0;
/// How far each page behind the first sits, and how many show at most.
const STACK_STEP: f32 = 3.0;
const STACK_MAX: usize = 3;
const OUTLINE: f32 = 1.25;

const PAPER: [f32; 3] = [0.984, 0.980, 0.965];
const FOLD_SHADE: [f32; 3] = [0.894, 0.874, 0.824];
const INK: [f32; 3] = [0.227, 0.220, 0.200];

/// A committed icon surface, kept alive for the length of the drag.
pub(crate) struct DragIcon {
    pub surface: wl_surface::WlSurface,
    buffer: wl_buffer::WlBuffer,
}

impl DragIcon {
    pub fn destroy(self) {
        self.surface.destroy();
        self.buffer.destroy();
    }
}

/// Makes the icon surface for `count` files at an integer output `scale`.
/// The buffer is attached later, once the surface has its drag-icon role.
pub(crate) fn create(
    globals: &Globals,
    qh: &QueueHandle<WaylandClientStatePtr>,
    count: usize,
    scale: i32,
) -> Option<(DragIcon, IconBuffer)> {
    let scale = scale.max(1);
    let (width, height, pixels) = draw(count, scale);
    let fd = memfd()?;
    let mut file = File::from(fd);
    file.write_all(&pixels).ok()?;
    let pool = globals
        .shm
        .create_pool(file.as_fd(), pixels.len() as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        width as i32 * 4,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    let surface = globals.compositor.create_surface(qh, ());
    Some((
        DragIcon {
            surface,
            buffer: buffer.clone(),
        },
        IconBuffer { buffer, scale },
    ))
}

/// What to attach once the drag has started.
pub(crate) struct IconBuffer {
    buffer: wl_buffer::WlBuffer,
    scale: i32,
}

impl IconBuffer {
    pub fn attach(self, surface: &wl_surface::WlSurface) {
        surface.set_buffer_scale(self.scale);
        surface.attach(Some(&self.buffer), 0, 0);
        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        surface.commit();
    }
}

fn memfd() -> Option<OwnedFd> {
    // SAFETY: the name is a NUL-terminated literal; the returned descriptor is
    // checked and then owned exactly once.
    let fd = unsafe { libc::memfd_create(c"gpui-drag-icon".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` was just created and is owned by no one else.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The icon's pixels: premultiplied ARGB8888, little-endian (B, G, R, A).
pub(crate) fn draw(count: usize, scale: i32) -> (u32, u32, Vec<u8>) {
    let pages = count.clamp(1, STACK_MAX);
    let stack = STACK_STEP * (pages - 1) as f32;
    let scale = scale as f32;
    let width = ((PAGE_WIDTH + stack + OUTLINE) * scale).ceil() as u32;
    let height = ((PAGE_HEIGHT + stack + OUTLINE) * scale).ceil() as u32;
    let mut pixels = vec![0_u8; (width * height * 4) as usize];
    const SAMPLES: usize = 4;
    for py in 0..height {
        for px in 0..width {
            let mut color = [0.0_f32; 4];
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / scale;
                    // The front page is drawn last, over the ones behind it.
                    let mut sample = None;
                    for page in (0..pages).rev() {
                        let offset = STACK_STEP * page as f32;
                        if let Some(rgb) = page_at(x - offset, y - offset) {
                            sample = Some(rgb);
                        }
                    }
                    if let Some(rgb) = sample {
                        color[0] += rgb[0];
                        color[1] += rgb[1];
                        color[2] += rgb[2];
                        color[3] += 1.0;
                    }
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let at = ((py * width + px) * 4) as usize;
            // Premultiplied: the summed colour over all samples is already
            // weighted by coverage.
            pixels[at] = (color[2] / n * 255.0).round() as u8;
            pixels[at + 1] = (color[1] / n * 255.0).round() as u8;
            pixels[at + 2] = (color[0] / n * 255.0).round() as u8;
            pixels[at + 3] = (color[3] / n * 255.0).round() as u8;
        }
    }
    (width, height, pixels)
}

/// The colour of one page at `(x, y)` (its own coordinates), if it covers it.
fn page_at(x: f32, y: f32) -> Option<[f32; 3]> {
    let (w, h, f, t) = (PAGE_WIDTH, PAGE_HEIGHT, FOLD, OUTLINE);
    if x < 0.0 || y < 0.0 || x >= w || y >= h {
        return None;
    }
    // The folded corner is cut away above the diagonal from (w - f, 0) to (w, f).
    let diagonal = x - (w - f) - y;
    if diagonal > 0.0 {
        return None;
    }
    let on_edge = x < t || y < t || x >= w - t || y >= h - t || diagonal > -t * 1.414;
    let in_fold = x >= w - f && y < f;
    let on_fold_edge = in_fold && (x < w - f + t || y >= f - t);
    if on_edge || on_fold_edge {
        Some(INK)
    } else if in_fold {
        Some(FOLD_SHADE)
    } else {
        Some(PAPER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(pixels: &[u8], width: u32, x: u32, y: u32) -> u8 {
        pixels[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn a_page_is_opaque_inside_and_clear_past_its_fold() {
        let (width, height, pixels) = draw(1, 1);
        assert_eq!((width, height), (28, 34));
        assert_eq!(alpha(&pixels, width, 10, 20), 255, "the page body");
        assert_eq!(alpha(&pixels, width, 25, 1), 0, "the cut corner");
        assert_eq!(alpha(&pixels, width, 27, 33), 0, "outside the page");
    }

    #[test]
    fn several_files_stack_and_scale_doubles_the_buffer() {
        let (one, _, _) = draw(1, 1);
        let (three, _, _) = draw(5, 1);
        assert_eq!(three, one + 6, "at most three pages, three pixels apart");
        let (double, _, _) = draw(1, 2);
        assert_eq!(double, 55);
    }
}
