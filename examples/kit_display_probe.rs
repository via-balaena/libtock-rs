//! Finds the display's backlight pin, which is the cheapest way into an
//! undocumented TFT.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_display_probe
//! ```
//!
//! Watch the screen while this runs. Nothing here talks SPI or knows what
//! controller the panel has; it drives one pin at a time and asks which one
//! makes the backlight come on. A lit backlight is the one display signal that
//! needs no initialisation sequence, no controller identification and no
//! agreement about pixel format — it is an LED and a series resistor.
//!
//! Knowing that pin is worth more than it sounds. It confirms the silkscreen's
//! `GP2-7` group really is the display, it takes one pin out of the six the
//! rest of the wiring has to be guessed from, and a screen that can be lit is a
//! screen that can be seen to be blank rather than assumed to be absent —
//! which is the difference between debugging a wiring guess and debugging an
//! initialisation sequence.
//!
//! Two passes. Each pin alone identifies the backlight outright if it is
//! independent, which it usually is. All six together is the fallback: if that
//! lights and no single pin did, the backlight is gated by something else on
//! the bus — a reset that has to be released first — and the next probe narrows
//! which, rather than this one guessing.
//!
//! On driving `GP4`, which the RP2350's pin table makes a candidate for SPI0 RX
//! and so possibly the panel's MISO: a controller only drives MISO while its
//! chip select is asserted, and nothing here asserts one, so it should be high
//! impedance throughout with nothing to contend with. That is an argument, not
//! a measurement, and it is why nothing outside the silkscreen's display group
//! is driven at all.

#![no_main]
#![no_std]

use core::fmt::Write;

use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x800}

/// The silkscreen's display group. Nothing outside it is touched.
const FIRST: u32 = 2;
const LAST: u32 = 7;

const HOLD_MS: u32 = 2500;
const GAP_MS: u32 = 500;

fn main() {
    let mut console = Console::writer();
    let _ = writeln!(
        console,
        "kit_display_probe: watch the screen, note when the backlight lights\r"
    );

    let _ = writeln!(console, "\r\npass 1 -- each pin alone\r");
    for number in FIRST..=LAST {
        drive_alone(&mut console, number);
    }

    let _ = writeln!(
        console,
        "\r\npass 2 -- all six at once, for a backlight that is gated by \
         something else\r"
    );
    drive_all(&mut console);
}

/// Holds one pin high, then releases it. The `OutputPin` is scoped so its
/// `Drop` hands the pin back before the next is driven — without that, pass one
/// would quietly turn into pass two.
fn drive_alone(console: &mut impl Write, number: u32) {
    let mut pin = match Gpio::get_pin(number) {
        Ok(pin) => pin,
        Err(_) => {
            let _ = writeln!(console, "  GP{number} not available\r");
            return;
        }
    };

    match pin.make_output() {
        Ok(mut output) => {
            let _ = writeln!(console, "  GP{number} high\r");
            let _ = output.set();
            let _ = Alarm::sleep_for(Milliseconds(HOLD_MS));
            let _ = output.clear();
        }
        Err(_) => {
            let _ = writeln!(console, "  GP{number} would not drive\r");
        }
    }

    let _ = Alarm::sleep_for(Milliseconds(GAP_MS));
}

/// Holds the whole group high together.
///
/// Written out rather than looped because each `OutputPin` borrows its own
/// `Pin`, and six live borrows is what "all at once" means. The pins are
/// released when this returns, so the screen going dark again is itself the
/// confirmation that these pins were what lit it.
fn drive_all(console: &mut impl Write) {
    let (mut p2, mut p3, mut p4, mut p5, mut p6, mut p7) = match (
        Gpio::get_pin(2),
        Gpio::get_pin(3),
        Gpio::get_pin(4),
        Gpio::get_pin(5),
        Gpio::get_pin(6),
        Gpio::get_pin(7),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e), Ok(f)) => (a, b, c, d, e, f),
        _ => {
            let _ = writeln!(console, "  not all of GP2-7 are available\r");
            return;
        }
    };

    let outputs = (
        p2.make_output(),
        p3.make_output(),
        p4.make_output(),
        p5.make_output(),
        p6.make_output(),
        p7.make_output(),
    );

    let (Ok(mut a), Ok(mut b), Ok(mut c), Ok(mut d), Ok(mut e), Ok(mut f)) = outputs else {
        let _ = writeln!(console, "  not all of GP2-7 would drive\r");
        return;
    };

    let _ = a.set();
    let _ = b.set();
    let _ = c.set();
    let _ = d.set();
    let _ = e.set();
    let _ = f.set();

    let _ = writeln!(console, "  GP2-7 all high, holding\r");
    let _ = Alarm::sleep_for(Milliseconds(HOLD_MS * 3));
    let _ = writeln!(console, "  releasing -- the screen should go dark again\r");
}
