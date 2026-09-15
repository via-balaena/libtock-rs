//! Measures an unknown panel's width and height by drawing markers where its
//! edges would be, and leaving them there.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_display_extent
//! ```
//!
//! Replaces a timed band test that asked the wrong question. That one streamed
//! pixels and assumed a row was `width` long, so a wrong guess was supposed to
//! shear the bands into diagonals -- but the panel fills down columns at
//! `MADCTL 0x00`, which makes every guess produce vertical bars and no guess
//! produce an answer. It also required watching a six-second window, which is a
//! bad thing to ask of an instrument when the alternative is standing still.
//!
//! This addresses pixels rather than streaming them. `CASET` and `RASET` set a
//! window, `RAMWR` fills it, and a controller clips or ignores a window outside
//! its panel -- so a marker that draws is a marker inside the display, and one
//! that stays black is off the end.
//!
//! Markers along the top say which width is real; markers down the left say
//! which height. Everything is drawn once and left up, so there is nothing to
//! catch and no order to remember.
//!
//!     top row, left to right      red at the origin, then
//!                                 GREEN if 240 wide
//!                                 BLUE  if 320 wide
//!                                 WHITE if 480 wide
//!
//!     left column, top to bottom  YELLOW  if 240 tall
//!                                 CYAN    if 320 tall
//!                                 MAGENTA if 480 tall
//!
//! Colours are the ones after `INVON`, which `kit_display_paint` established
//! this panel needs.

#![no_main]
#![no_std]

use core::fmt::Write;

use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio::{Gpio, OutputPin};
use libtock::runtime::{set_main, stack_size, TockSyscalls};
use libtock::spi_controller::SpiController;
use libtock_platform::ErrorCode;

set_main! {main}
stack_size! {0x800}

type Out<'a> = OutputPin<'a, TockSyscalls>;

const CS: u32 = 5;
const DC: u32 = 6;
const RST: u32 = 7;
const RATE_HZ: u32 = 8_000_000;

const SWRESET: u8 = 0x01;
const SLPOUT: u8 = 0x11;
const INVON: u8 = 0x21;
const DISPON: u8 = 0x29;
const CASET: u8 = 0x2a;
const RASET: u8 = 0x2b;
const RAMWR: u8 = 0x2c;
const MADCTL: u8 = 0x36;
const COLMOD: u8 = 0x3a;

const BLACK: [u8; 2] = [0x00, 0x00];
const RED: [u8; 2] = [0xf8, 0x00];
const GREEN: [u8; 2] = [0x07, 0xe0];
const BLUE: [u8; 2] = [0x00, 0x1f];
const WHITE: [u8; 2] = [0xff, 0xff];
const YELLOW: [u8; 2] = [0xff, 0xe0];
const CYAN: [u8; 2] = [0x07, 0xff];
const MAGENTA: [u8; 2] = [0xf8, 0x1f];

/// Marker size. Big enough to see from across a room.
const M: u16 = 40;

fn main() {
    let mut console = Console::writer();

    if SpiController::exists().is_err() {
        let _ = writeln!(console, "kit_display_extent: no SPI driver");
        return;
    }
    let _ = SpiController::set_baud_rate(RATE_HZ);

    let (mut cs_pin, mut dc_pin, mut rst_pin) =
        match (Gpio::get_pin(CS), Gpio::get_pin(DC), Gpio::get_pin(RST)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            _ => {
                let _ = writeln!(console, "kit_display_extent: control pins unavailable");
                return;
            }
        };
    let (mut cs, mut dc, mut rst) = match (
        cs_pin.make_output(),
        dc_pin.make_output(),
        rst_pin.make_output(),
    ) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        _ => {
            let _ = writeln!(console, "kit_display_extent: could not drive control pins");
            return;
        }
    };
    let _ = cs.set();
    let _ = dc.set();

    reset(&mut rst);
    if init(&mut cs, &mut dc).is_err() {
        let _ = writeln!(console, "kit_display_extent: init failed\r");
        return;
    }

    // A black field first, so a marker that does not draw is visibly absent
    // rather than lost in whatever was on the panel before.
    let _ = writeln!(console, "kit_display_extent: clearing\r");
    let _ = fill_rect(&mut cs, &mut dc, 0, 0, 479, 479, BLACK);

    let _ = writeln!(console, "kit_display_extent: drawing markers\r");

    // Width markers along the top.
    let _ = fill_rect(&mut cs, &mut dc, 0, 0, M - 1, M - 1, RED);
    let _ = fill_rect(&mut cs, &mut dc, 240 - M, 0, 239, M - 1, GREEN);
    let _ = fill_rect(&mut cs, &mut dc, 320 - M, 0, 319, M - 1, BLUE);
    let _ = fill_rect(&mut cs, &mut dc, 480 - M, 0, 479, M - 1, WHITE);

    // Height markers down the left.
    let _ = fill_rect(&mut cs, &mut dc, 0, 240 - M, M - 1, 239, YELLOW);
    let _ = fill_rect(&mut cs, &mut dc, 0, 320 - M, M - 1, 319, CYAN);
    let _ = fill_rect(&mut cs, &mut dc, 0, 480 - M, M - 1, 479, MAGENTA);

    let _ = writeln!(
        console,
        "\r\nkit_display_extent: markers are up and stay up.\r\n\
         across the top: red at the corner, then GREEN=240, BLUE=320, WHITE=480 wide\r\n\
         down the left:  YELLOW=240, CYAN=320, MAGENTA=480 tall\r\n\
         The last one visible on each edge is the size.\r"
    );

    loop {
        let _ = Alarm::sleep_for(Milliseconds(1000));
    }
}

fn reset(rst: &mut Out<'_>) {
    let _ = rst.set();
    let _ = Alarm::sleep_for(Milliseconds(20));
    let _ = rst.clear();
    let _ = Alarm::sleep_for(Milliseconds(20));
    let _ = rst.set();
    let _ = Alarm::sleep_for(Milliseconds(150));
}

fn init(cs: &mut Out<'_>, dc: &mut Out<'_>) -> Result<(), ErrorCode> {
    command(cs, dc, SWRESET, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(150));
    command(cs, dc, SLPOUT, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(150));
    command(cs, dc, COLMOD, &[0x55])?;
    command(cs, dc, MADCTL, &[0x00])?;
    command(cs, dc, INVON, &[])?;
    command(cs, dc, DISPON, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(50));
    Ok(())
}

/// Sets a window and fills it. A window past the panel's edge is the controller's
/// problem, and its answer -- clip, wrap or ignore -- is what this is reading.
fn fill_rect(
    cs: &mut Out<'_>,
    dc: &mut Out<'_>,
    x0: u16,
    y0: u16,
    x1: u16,
    y1: u16,
    colour: [u8; 2],
) -> Result<(), ErrorCode> {
    command(
        cs,
        dc,
        CASET,
        &[(x0 >> 8) as u8, x0 as u8, (x1 >> 8) as u8, x1 as u8],
    )?;
    command(
        cs,
        dc,
        RASET,
        &[(y0 >> 8) as u8, y0 as u8, (y1 >> 8) as u8, y1 as u8],
    )?;
    command(cs, dc, RAMWR, &[])?;

    let pixels = (x1 - x0 + 1) as usize * (y1 - y0 + 1) as usize;
    let mut chunk = [0u8; 256];
    for pair in chunk.chunks_exact_mut(2) {
        pair.copy_from_slice(&colour);
    }

    let _ = cs.clear();
    let _ = dc.set();
    let mut sent = 0usize;
    while sent < pixels {
        let take = (pixels - sent).min(128);
        if SpiController::spi_controller_write_sync(&chunk, (take * 2) as u32).is_err() {
            let _ = cs.set();
            return Err(ErrorCode::Fail);
        }
        sent += take;
    }
    let _ = cs.set();
    Ok(())
}

fn command(cs: &mut Out<'_>, dc: &mut Out<'_>, code: u8, args: &[u8]) -> Result<(), ErrorCode> {
    let _ = cs.clear();
    let _ = dc.clear();
    let sent = SpiController::spi_controller_write_sync(&[code], 1);
    if !args.is_empty() {
        let _ = dc.set();
        let _ = SpiController::spi_controller_write_sync(args, args.len() as u32);
    }
    let _ = cs.set();
    sent
}
