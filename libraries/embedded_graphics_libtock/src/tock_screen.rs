//! Implementations of `DrawTarget` using the screen system call.

use libtock::display::Screen;
use libtock_platform::ErrorCode;

/// An implementation of a `DrawTarget` for monochromatic, 128x64 pixel screens
/// where the pixels in each byte are vertical on the screen.
///
/// This corresponds to the `Mono_8BitPage` pixel format documented
/// [here](https://github.com/tock/tock/blob/master/doc/syscalls/90001_screen.md#command-number-25).
pub struct TockMonochrome8BitPage128x64Screen {
    /// The framebuffer for the max supported screen size (128x64). Each pixel
    /// is a bit.
    framebuffer: [u8; (128 * 64) / 8],
    width: u32,
    height: u32,
    /// Whether this screen can actually display what `flush` will send it.
    ///
    /// `new` cannot return a `Result` without removing `Default`, so the
    /// answer is kept here and reported by `flush` and `setup_result`.
    setup: Result<(), ErrorCode>,
}

/// Pixel formats this adapter's framebuffer can drive. It packs one bit per
/// pixel, eight vertical pixels to a byte, so a mono format of either spelling
/// takes these bytes unchanged.
///
/// Both are listed because they are the same format either side of one kernel
/// commit. Tock `35ca7fa07` (2025-08-04) added `Mono_8BitPage = 6` to the HIL
/// and moved `ssd1306` and `sh1106` off `Mono = 0` — same hardware, same buffer
/// layout, different number. So a kernel built before that date reports 0 and
/// one built after reports 6, and checking against either alone rejects the
/// panel this adapter is named for on half the kernels in existence.
///
/// **This is a pin boundary, not a fork difference.** Both drivers are
/// identical to upstream on both sides of it, so do not go looking for a
/// divergence to blame. The 0 can be dropped once libtock-rs pins tock past
/// 2025-08-04.
const MONO_FORMATS: [u32; 2] = [0, 6];

impl Default for TockMonochrome8BitPage128x64Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl TockMonochrome8BitPage128x64Screen {
    pub fn new() -> Self {
        // DO NOT ask the driver to CHANGE its format, and do not judge this
        // screen by what it answers if you do. No driver in tree can change
        // one: `ssd1306` answers NOSUPPORT to every format including the one
        // it already uses, and `st77xx` answers INVAL to everything but
        // RGB_565 -- and BUSY, for any format at all, while it is still
        // initialising. None of those answers says whether this adapter's
        // bytes will be interpreted correctly, which is the only question.
        //
        // Ask that question directly instead. `get_pixel_format` reads the
        // driver's own getter with no state check, no queue and no upcall, so
        // it is answerable at any point in boot and cannot be deferred.
        let (setup, width, height) = match Screen::get_resolution() {
            Ok((width, height)) => match Screen::get_pixel_format() {
                Ok(format) if MONO_FORMATS.contains(&format) => (Ok(()), width, height),
                Ok(_) => (Err(ErrorCode::NoSupport), width, height),
                Err(e) => (Err(e), width, height),
            },
            // A resolution this could not read leaves a 0x0 write frame with
            // 1024 bytes pushed into it.
            Err(e) => (Err(e), 0, 0),
        };

        Self {
            framebuffer: [0; 1024],
            width,
            height,
            setup,
        }
    }

    /// Whether this screen reported a resolution and a format this adapter
    /// can drive.
    ///
    /// `flush` answers the same error, so this is only needed to fail before
    /// drawing rather than at the first draw.
    pub fn setup_result(&self) -> Result<(), ErrorCode> {
        self.setup
    }

    pub fn get_width(&self) -> u32 {
        self.width
    }

    pub fn get_height(&self) -> u32 {
        self.height
    }

    /// Updates the screen from the framebuffer.
    ///
    /// Answers the constructor's error first: drawing into a screen that
    /// refused this adapter's pixel format produces a pattern rather than a
    /// failure, which is worse than not drawing.
    pub fn flush(&self) -> Result<(), ErrorCode> {
        self.setup?;
        Screen::set_write_frame(0, 0, self.width, self.height)?;
        Screen::write(&self.framebuffer)?;
        Ok(())
    }
}

impl embedded_graphics::draw_target::DrawTarget for TockMonochrome8BitPage128x64Screen {
    type Color = embedded_graphics::pixelcolor::BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>>,
    {
        for embedded_graphics::Pixel(coord, color) in pixels.into_iter() {
            if coord.x >= 0
                && coord.x < self.width as i32
                && coord.y >= 0
                && coord.y < self.height as i32
            {
                const X_FACTOR: usize = 1;
                const Y_FACTOR: usize = 8;
                const X_COLS: usize = 128;

                let x = coord.x as usize;
                let y = coord.y as usize;

                let byte_index = (x / X_FACTOR) + ((y / Y_FACTOR) * X_COLS);
                let bit_index = y % Y_FACTOR;

                if color.is_on() {
                    self.framebuffer[byte_index] |= 1 << bit_index;
                } else {
                    self.framebuffer[byte_index] &= !(1 << bit_index);
                }
            }
        }

        Ok(())
    }
}

impl embedded_graphics::geometry::OriginDimensions for TockMonochrome8BitPage128x64Screen {
    fn size(&self) -> embedded_graphics::geometry::Size {
        embedded_graphics::geometry::Size::new(128, 64)
    }
}
