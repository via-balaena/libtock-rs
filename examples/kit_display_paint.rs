//! Tries four display controllers in turn and paints the screen a different
//! colour for each. Whichever colour appears names the panel.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_display_paint
//! ```
//!
//! `kit_display_id` asked the panel what it was and got `ff` back at every
//! clock rate, which is what an unconnected MISO looks like — plenty of these
//! modules buffer the control lines one way and never drive it. So this stops
//! asking and starts telling: initialise as if it were each candidate, fill
//! with a colour unique to that guess, and let the screen answer.
//!
//! | guess    | colour |
//! |----------|--------|
//! | ILI9341  | red    |
//! | ILI9486  | green  |
//! | ST7796   | blue   |
//! | ST7789   | white  |
//!
//! A wrong guess usually shows nothing; it may show noise, which is still an
//! answer, because it means the wires carry and the init was merely wrong.
//!
//! No window is set. All four controllers put their address counter at the
//! origin and their window at full screen after reset, so streaming pixels
//! straight after `RAMWR` fills from the top left without this having to know
//! the panel's dimensions — which is the thing it is trying to find out.
//! Enough pixels are sent to cover the largest candidate.
//!
//! Nothing here can damage the panel: every byte is a register write or pixel
//! data, and a controller that does not recognise a command ignores it.

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

/// Fast enough to fill a screen in under a second, slow enough that a marginal
/// wire still works. Writes tolerate far more than reads.
const RATE_HZ: u32 = 8_000_000;

/// Covers 480x320, the largest candidate, so no dimension has to be guessed.
const PIXELS: usize = 480 * 320;
/// Two bytes a pixel at 16bpp; the chunk is a whole number of pixels.
const CHUNK: usize = 256;

/// Commands every one of these controllers shares.
const SWRESET: u8 = 0x01;
const SLPOUT: u8 = 0x11;
const INVON: u8 = 0x21;
const DISPON: u8 = 0x29;
const RAMWR: u8 = 0x2c;
const MADCTL: u8 = 0x36;
const COLMOD: u8 = 0x3a;

struct Candidate {
    name: &'static str,
    colour: [u8; 2],
    /// Whether this controller wants inversion on, which ST7789 does and the
    /// others do not.
    invert: bool,
    madctl: u8,
}

const CANDIDATES: [Candidate; 4] = [
    Candidate {
        name: "ILI9341 (red)",
        colour: [0xf8, 0x00],
        invert: false,
        madctl: 0x48,
    },
    Candidate {
        name: "ILI9486 (green)",
        colour: [0x07, 0xe0],
        invert: false,
        madctl: 0x48,
    },
    Candidate {
        name: "ST7796 (blue)",
        colour: [0x00, 0x1f],
        invert: false,
        madctl: 0x48,
    },
    Candidate {
        name: "ST7789 (white)",
        colour: [0xff, 0xff],
        invert: true,
        madctl: 0x00,
    },
];

fn main() {
    let mut console = Console::writer();

    if SpiController::exists().is_err() {
        let _ = writeln!(console, "kit_display_paint: no SPI driver on this kernel");
        return;
    }
    let _ = SpiController::set_baud_rate(RATE_HZ);
    let actual = SpiController::get_baud_rate().unwrap_or(0);

    let (mut cs_pin, mut dc_pin, mut rst_pin) =
        match (Gpio::get_pin(CS), Gpio::get_pin(DC), Gpio::get_pin(RST)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            _ => {
                let _ = writeln!(console, "kit_display_paint: control pins unavailable");
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
            let _ = writeln!(console, "kit_display_paint: could not drive control pins");
            return;
        }
    };
    let _ = cs.set();
    let _ = dc.set();

    let _ = writeln!(
        console,
        "kit_display_paint: bus at {actual} Hz, watch the screen\r"
    );

    for candidate in CANDIDATES {
        let _ = writeln!(console, "\r\ntrying {}\r", candidate.name);

        reset(&mut rst);
        if init(&mut cs, &mut dc, &candidate).is_err() {
            let _ = writeln!(console, "  init failed\r");
            continue;
        }
        match fill(&mut cs, &mut dc, candidate.colour) {
            Ok(sent) => {
                let _ = writeln!(console, "  filled, {sent} pixels sent\r");
            }
            Err(_) => {
                let _ = writeln!(console, "  fill failed\r");
            }
        }

        // Long enough to notice and say which colour it was.
        let _ = Alarm::sleep_for(Milliseconds(5000));
    }

    let _ = writeln!(
        console,
        "\r\nkit_display_paint: done. The colour that appeared names the panel; \
         noise means the wires carry and only the init was wrong.\r"
    );
}

fn reset(rst: &mut Out<'_>) {
    let _ = rst.set();
    let _ = Alarm::sleep_for(Milliseconds(20));
    let _ = rst.clear();
    let _ = Alarm::sleep_for(Milliseconds(20));
    let _ = rst.set();
    let _ = Alarm::sleep_for(Milliseconds(150));
}

/// The short init every one of these accepts: out of sleep, 16 bits a pixel,
/// a known scan order, display on. The long magic-register sequences in vendor
/// drivers tune gamma and power; none of them is needed to get a picture.
fn init(cs: &mut Out<'_>, dc: &mut Out<'_>, candidate: &Candidate) -> Result<(), ErrorCode> {
    command(cs, dc, SWRESET, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(150));

    command(cs, dc, SLPOUT, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(150));

    command(cs, dc, COLMOD, &[0x55])?;
    command(cs, dc, MADCTL, &[candidate.madctl])?;
    if candidate.invert {
        command(cs, dc, INVON, &[])?;
    }
    command(cs, dc, DISPON, &[])?;
    let _ = Alarm::sleep_for(Milliseconds(50));
    Ok(())
}

/// Streams one colour from the origin, enough to cover the largest candidate.
fn fill(cs: &mut Out<'_>, dc: &mut Out<'_>, colour: [u8; 2]) -> Result<usize, ErrorCode> {
    let mut chunk = [0u8; CHUNK];
    for pair in chunk.chunks_exact_mut(2) {
        pair.copy_from_slice(&colour);
    }

    command(cs, dc, RAMWR, &[])?;

    // One chip select for the whole stream: the controller takes pixel data
    // until something else interrupts it, and re-asserting between chunks is
    // what would.
    let _ = cs.clear();
    let _ = dc.set();
    let mut sent = 0;
    while sent < PIXELS {
        if SpiController::spi_controller_write_sync(&chunk, CHUNK as u32).is_err() {
            let _ = cs.set();
            return Err(ErrorCode::Fail);
        }
        sent += CHUNK / 2;
    }
    let _ = cs.set();
    Ok(sent)
}

/// One command byte with DC low, then its parameters with DC high, inside one
/// chip select.
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
