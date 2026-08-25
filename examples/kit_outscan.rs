//! Output walk for an undocumented Pico Breadboard Kit.
//!
//! The earlier probe only ever read pins. Anything the Pico *drives* -- an LED,
//! a buzzer, a backlight enable -- reads as "tied high" like everything else,
//! so an input scan cannot find it. This drives each pin high for a moment and
//! announces which one, so whatever lights up or makes noise identifies itself.
//!
//! Deliberately skipped:
//!   GP0, GP1    the console
//!   GP14, GP15  buttons -- driving a pin high while its button shorts it to
//!               ground is a short through the pin driver
//!   GP23-25, 29 CYW43 and the VSYS divider on a Pico 2 W
//!
//! GP13 (beeper) and GP16/GP17 (LEDs) are included on purpose as known
//! positives: if those do not react, the method is broken rather than the board.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x800}

const PINS: [u32; 20] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20, 21, 22, 26,
];

fn main() {
    let _ = writeln!(Console::writer(), "\r\n=== output walk ===\r");
    let _ = writeln!(
        Console::writer(),
        "each pin driven HIGH for 2s. watch for light, listen for the beeper.\r"
    );

    loop {
        for p in PINS {
            let mut pin = match Gpio::get_pin(p) {
                Ok(pin) => pin,
                Err(_) => continue,
            };
            let mut out = match pin.make_output() {
                Ok(o) => o,
                Err(_) => {
                    let _ = writeln!(Console::writer(), "  GP{p:<2}  cannot drive\r");
                    continue;
                }
            };

            let _ = writeln!(Console::writer(), "  GP{p:<2}  HIGH\r");
            let _ = out.set();
            let _ = Alarm::sleep_for(Milliseconds(2000));
            let _ = out.clear();
            let _ = Alarm::sleep_for(Milliseconds(400));
        }
        let _ = writeln!(Console::writer(), "  --- pass complete, repeating ---\r");
        let _ = Alarm::sleep_for(Milliseconds(2000));
    }
}
