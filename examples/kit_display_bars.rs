//! Colour bars, the right way up, on the kit's 320x480 panel.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_display_bars
//! ```
//!
//! Everything here was measured rather than looked up, because the module has
//! no legible part number and does not connect MISO:
//!
//! * `kit_display_id` got `ff` back at every clock rate, so the panel cannot be
//!   asked anything and every fact has to be visible.
//! * `kit_display_paint` filled it four times; the first three came out as
//!   exact colour complements and the fourth, with `INVON` set, came out true.
//!   So the panel is IPS and wants inversion.
//! * `kit_display_extent` drew markers where each candidate edge would be. The
//!   marker at 440-479 on one axis never appeared and the one at 280-319 did,
//!   so that axis is 320; the other reached 480. **320 x 480**, and the markers
//!   came out along the wrong walls, so the kit mounts a portrait panel in
//!   landscape.
//!
//! Which leaves ILI9486 or ST7796. Not ILI9488: that one cannot do 16-bit
//! colour over SPI, it wants three bytes a pixel, and two-byte pixels drew.
//!
//! `MADCTL` gets `MV` here, exchanging rows and columns, which turns the native
//! portrait into the landscape the kit mounts. If the bars come out mirrored,
//! `MX` or `MY` is the bit to add -- the pattern is deliberately asymmetric in
//! both axes so that is visible rather than a matter of opinion.

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

/// `MV` exchanges rows and columns, turning the native portrait into the
/// landscape the kit mounts. `BGR` says the panel's subpixels are in that
/// order: without it red and blue come out exchanged, which is measured rather
/// than assumed -- a bar sent yellow came back cyan, cyan came back yellow, and
/// red and blue traded, while white, green, magenta and black were unmoved.
/// That is exactly the set that survives swapping the red and blue channels.
const LANDSCAPE: u8 = 0x20 | 0x08;

const WIDTH: u16 = 480;
const HEIGHT: u16 = 320;
/// The bars stop here; below is a strip that says which way up the screen is.
const BAR_BOTTOM: u16 = 279;

const BLACK: [u8; 2] = [0x00, 0x00];
const WHITE: [u8; 2] = [0xff, 0xff];
const YELLOW: [u8; 2] = [0xff, 0xe0];
const CYAN: [u8; 2] = [0x07, 0xff];
const GREEN: [u8; 2] = [0x07, 0xe0];
const MAGENTA: [u8; 2] = [0xf8, 0x1f];
const RED: [u8; 2] = [0xf8, 0x00];
const BLUE: [u8; 2] = [0x00, 0x1f];

/// The broadcast order, left to right.
const BARS: [([u8; 2], &str); 8] = [
    (WHITE, "white"),
    (YELLOW, "yellow"),
    (CYAN, "cyan"),
    (GREEN, "green"),
    (MAGENTA, "magenta"),
    (RED, "red"),
    (BLUE, "blue"),
    (BLACK, "black"),
];

fn main() {
    let mut console = Console::writer();

    if SpiController::exists().is_err() {
        let _ = writeln!(console, "kit_display_bars: no SPI driver");
        return;
    }
    let _ = SpiController::set_baud_rate(RATE_HZ);

    let (mut cs_pin, mut dc_pin, mut rst_pin) =
        match (Gpio::get_pin(CS), Gpio::get_pin(DC), Gpio::get_pin(RST)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            _ => {
                let _ = writeln!(console, "kit_display_bars: control pins unavailable");
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
            let _ = writeln!(console, "kit_display_bars: could not drive control pins");
            return;
        }
    };
    let _ = cs.set();
    let _ = dc.set();

    reset(&mut rst);
    if init(&mut cs, &mut dc).is_err() {
        let _ = writeln!(console, "kit_display_bars: init failed\r");
        return;
    }

    let bar = WIDTH / BARS.len() as u16;
    for (index, (colour, name)) in BARS.iter().enumerate() {
        let x0 = bar * index as u16;
        let x1 = x0 + bar - 1;
        let _ = write!(console, "{name} ");
        let _ = fill_rect(&mut cs, &mut dc, x0, 0, x1, BAR_BOTTOM, *colour);
    }
    let _ = writeln!(console, "\r");

    // Half a strip along the bottom. Asymmetric on purpose: it says which edge
    // is the bottom and which side is the left, so a mirrored or flipped
    // rotation is visible rather than arguable.
    let _ = fill_rect(
        &mut cs,
        &mut dc,
        0,
        BAR_BOTTOM + 1,
        WIDTH / 2 - 1,
        HEIGHT - 1,
        BLUE,
    );
    let _ = fill_rect(
        &mut cs,
        &mut dc,
        WIDTH / 2,
        BAR_BOTTOM + 1,
        WIDTH - 1,
        HEIGHT - 1,
        WHITE,
    );

    let _ = writeln!(
        console,
        "kit_display_bars: 8 bars, left to right white yellow cyan green \
         magenta red blue black,\r\n\
         and a strip along the bottom that is blue on the left, white on the right.\r"
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
    command(cs, dc, MADCTL, &[LANDSCAPE])?;
    command(cs, dc, INVON, &[])?;
    command(cs, dc, DISPON, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(50));
    Ok(())
}

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
