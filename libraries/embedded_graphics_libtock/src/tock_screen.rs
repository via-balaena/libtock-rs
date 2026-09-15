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
    /// Whether the kernel accepted what `new` asked it for.
    ///
    /// `new` cannot return a `Result` without removing `Default`, and a screen
    /// whose pixel format was refused cannot draw anything correct, so the
    /// error is kept here and answered by `flush` and `setup_result`.
    setup: Result<(), ErrorCode>,
}

impl Default for TockMonochrome8BitPage128x64Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl TockMonochrome8BitPage128x64Screen {
    pub fn new() -> Self {
        // Because this is a specific type of screen with a specific pixel
        // format, we tell the kernel that is the pixel format we expect.
        //
        // Both of these can fail, and neither failure is safe to discard. A
        // driver that refuses the format keeps the one it already has, and
        // every byte written afterwards is then interpreted in that other
        // format; a resolution this could not read leaves a 0x0 write frame
        // that 1024 bytes are pushed into. Verified on the Raspberry Pi Pico
        // 2's ST7796, where `st77xx::set_pixel_format` answers INVAL for
        // everything except RGB_565 -- so asking for Mono_8BitPage there and
        // ignoring the answer draws a monochrome framebuffer into an RGB565
        // panel.
        const MONO_8_BIT_PAGE: usize = 6;
        let (setup, width, height) = match Screen::get_resolution() {
            Ok((width, height)) => (Screen::set_pixel_format(MONO_8_BIT_PAGE), width, height),
            Err(e) => (Err(e), 0, 0),
        };

        Self {
            framebuffer: [0; 1024],
            width,
            height,
            setup,
        }
    }

    /// Whether the kernel accepted this screen's resolution query and pixel
    /// format.
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
