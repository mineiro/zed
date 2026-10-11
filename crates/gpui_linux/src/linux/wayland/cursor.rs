use crate::linux::{DEFAULT_CURSOR_ICON_NAME, log_cursor_icon_warning};
use crate::linux::{Globals, WaylandClientStatePtr};
use anyhow::{Context as _, anyhow};
use gpui_util::ResultExt;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};

use gpui::CustomCursor;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_pointer::WlPointer, wl_shm, wl_shm::WlShm};
use wayland_client::{Connection, QueueHandle};
use wayland_cursor::{CursorImageBuffer, CursorTheme};

pub(crate) struct Cursor {
    loaded_theme: Option<LoadedTheme>,
    size: u32,
    scaled_size: u32,
    surface: WlSurface,
    shm: WlShm,
    connection: Connection,
    qh: QueueHandle<WaylandClientStatePtr>,
    /// Buffers for application cursor images, made on first use.
    custom: HashMap<CustomCursor, CustomBuffer>,
}

struct CustomBuffer {
    buffer: WlBuffer,
    width: i32,
    height: i32,
    scale: i32,
    hotspot: (i32, i32),
}

pub(crate) struct LoadedTheme {
    theme: CursorTheme,
    name: Option<String>,
    scaled_size: u32,
}

impl Drop for Cursor {
    fn drop(&mut self) {
        self.loaded_theme.take();
        for (_, custom) in self.custom.drain() {
            custom.buffer.destroy();
        }
        self.surface.destroy();
    }
}

impl Cursor {
    pub fn new(connection: &Connection, globals: &Globals, size: u32) -> Self {
        let mut this = Self {
            loaded_theme: None,
            size,
            scaled_size: size,
            surface: globals.compositor.create_surface(&globals.qh, ()),
            shm: globals.shm.clone(),
            connection: connection.clone(),
            qh: globals.qh.clone(),
            custom: HashMap::new(),
        };
        this.set_theme_internal(None);
        this
    }

    fn set_theme_internal(&mut self, theme_name: Option<String>) {
        if let Some(loaded_theme) = self.loaded_theme.as_ref()
            && loaded_theme.name == theme_name
            && loaded_theme.scaled_size == self.scaled_size
        {
            return;
        }
        let result = if let Some(theme_name) = theme_name.as_ref() {
            CursorTheme::load_from_name(
                &self.connection,
                self.shm.clone(),
                theme_name,
                self.scaled_size,
            )
        } else {
            CursorTheme::load(&self.connection, self.shm.clone(), self.scaled_size)
        };
        if let Some(theme) = result
            .context("Wayland: Failed to load cursor theme")
            .log_err()
        {
            self.loaded_theme = Some(LoadedTheme {
                theme,
                name: theme_name,
                scaled_size: self.scaled_size,
            });
        }
    }

    pub fn set_theme(&mut self, theme_name: String) {
        self.set_theme_internal(Some(theme_name));
    }

    fn set_scaled_size(&mut self, scaled_size: u32) {
        self.scaled_size = scaled_size;
        let theme_name = self
            .loaded_theme
            .as_ref()
            .and_then(|loaded_theme| loaded_theme.name.clone());
        self.set_theme_internal(theme_name);
    }

    pub fn set_size(&mut self, size: u32) {
        self.size = size;
        self.set_scaled_size(size);
    }

    /// Shows an application's cursor image. False when it isn't registered
    /// or its buffer couldn't be made, so the caller can fall back.
    pub fn set_image(
        &mut self,
        wl_pointer: &WlPointer,
        serial_id: u32,
        cursor: CustomCursor,
    ) -> bool {
        if !self.custom.contains_key(&cursor) {
            let Some(made) = cursor.image().and_then(|image| self.make_buffer(&image)) else {
                return false;
            };
            self.custom.insert(cursor, made);
        }
        let custom = &self.custom[&cursor];
        // The buffer first, then the role: some compositors read the
        // surface once, when it becomes the cursor.
        self.surface.set_buffer_scale(custom.scale);
        self.surface.attach(Some(&custom.buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, custom.width, custom.height);
        self.surface.commit();
        wl_pointer.set_cursor(
            serial_id,
            Some(&self.surface),
            custom.hotspot.0,
            custom.hotspot.1,
        );
        true
    }

    fn make_buffer(&self, image: &gpui::CursorImage) -> Option<CustomBuffer> {
        let (width, height) = (image.width as i32, image.height as i32);
        if width <= 0 || height <= 0 || image.rgba.len() != (width * height * 4) as usize {
            log::warn!("Wayland: a custom cursor's pixels don't match its size");
            return None;
        }
        // A buffer's size must be a multiple of its scale.
        let scale = (image.scale as i32).max(1);
        if width % scale != 0 || height % scale != 0 {
            log::warn!("Wayland: a custom cursor's size isn't a multiple of its scale");
            return None;
        }
        // Premultiplied ARGB8888, stored little-endian as B, G, R, A.
        let mut pixels = Vec::with_capacity(image.rgba.len());
        for pixel in image.rgba.chunks_exact(4) {
            let alpha = u32::from(pixel[3]);
            let premultiply = |c: u8| ((u32::from(c) * alpha + 127) / 255) as u8;
            pixels.extend_from_slice(&[
                premultiply(pixel[2]),
                premultiply(pixel[1]),
                premultiply(pixel[0]),
                pixel[3],
            ]);
        }
        let fd = memfd()?;
        let mut file = File::from(fd);
        file.write_all(&pixels).ok()?;
        let pool = self
            .shm
            .create_pool(file.as_fd(), pixels.len() as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        Some(CustomBuffer {
            buffer,
            width,
            height,
            scale,
            hotspot: (image.hotspot.0 as i32, image.hotspot.1 as i32),
        })
    }

    pub fn set_icon(
        &mut self,
        wl_pointer: &WlPointer,
        serial_id: u32,
        cursor_icon_names: &[&str],
        scale: i32,
    ) {
        self.set_scaled_size(self.size * scale as u32);

        let Some(loaded_theme) = &mut self.loaded_theme else {
            log::warn!("Wayland: Unable to load cursor themes");
            return;
        };
        let theme = &mut loaded_theme.theme;

        let buffer: &CursorImageBuffer;
        'outer: {
            for cursor_icon_name in cursor_icon_names {
                if let Some(cursor) = theme.get_cursor(cursor_icon_name) {
                    buffer = &cursor[0];
                    break 'outer;
                }
            }

            if let Some(cursor) = theme.get_cursor(DEFAULT_CURSOR_ICON_NAME) {
                buffer = &cursor[0];
                log_cursor_icon_warning(anyhow!(
                    "wayland: Unable to get cursor icon {:?}. \
                    Using default cursor icon: '{}'",
                    cursor_icon_names,
                    DEFAULT_CURSOR_ICON_NAME
                ));
            } else {
                log_cursor_icon_warning(anyhow!(
                    "wayland: Unable to fallback on default cursor icon '{}' for theme '{}'",
                    DEFAULT_CURSOR_ICON_NAME,
                    loaded_theme.name.as_deref().unwrap_or("default")
                ));
                return;
            }
        }

        let (width, height) = buffer.dimensions();
        let (hot_x, hot_y) = buffer.hotspot();

        self.surface.set_buffer_scale(scale);

        wl_pointer.set_cursor(
            serial_id,
            Some(&self.surface),
            hot_x as i32 / scale,
            hot_y as i32 / scale,
        );

        self.surface.attach(Some(buffer), 0, 0);
        self.surface.damage(0, 0, width as i32, height as i32);
        self.surface.commit();
    }
}

fn memfd() -> Option<OwnedFd> {
    // SAFETY: the name is a NUL-terminated literal; the returned descriptor is
    // checked and then owned exactly once.
    let fd = unsafe { libc::memfd_create(c"gpui-cursor".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` was just created and is owned by no one else.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}
