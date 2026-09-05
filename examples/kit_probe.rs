//! Connectivity scanner for an undocumented Pico Breadboard Kit.
//!
//! Each pin is read twice, once with the internal pull-up and once with the
//! pull-down, allowing time to settle between. A pin that follows the pull is
//! floating; one that refuses is tied to something stronger.
//!
//! The scan repeats, so a capture started at any moment gets the whole table
//! rather than having to win a race against the board booting.
//!
//! Nothing is driven as an output. In the C version an earlier draft used two
//! pins for a heartbeat, which made anything wired to them invisible by
//! construction -- here that mistake would not compile, because `read` does not
//! exist on an `OutputPin`.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio::{self, Gpio, GpioState};
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x800}

const FIRST_PIN: u32 = 2;
const LAST_PIN: u32 = 29;
const SETTLE_MS: u32 = 20;

fn main() {
    let mut round: u32 = 0;

    loop {
        round += 1;
        let _ = writeln!(Console::writer(), "\r\n=== scan {round} ===\r");

        for p in FIRST_PIN..=LAST_PIN {
            let pin = match Gpio::get_pin(p) {
                Ok(pin) => pin,
                Err(_) => {
                    let _ = writeln!(Console::writer(), "  GP{p:<2}  not exposed\r");
                    continue;
                }
            };

            let up = match pin.make_input::<gpio::PullUp>() {
                Ok(input) => {
                    let _ = Alarm::sleep_for(Milliseconds(SETTLE_MS));
                    input.read()
                }
                Err(_) => continue,
            };
            let down = match pin.make_input::<gpio::PullDown>() {
                Ok(input) => {
                    let _ = Alarm::sleep_for(Milliseconds(SETTLE_MS));
                    input.read()
                }
                Err(_) => continue,
            };

            let verdict = match (up, down) {
                (Ok(GpioState::High), Ok(GpioState::Low)) => "floating",
                (Ok(GpioState::Low), Ok(GpioState::Low)) => "TIED LOW",
                (Ok(GpioState::High), Ok(GpioState::High)) => "TIED HIGH",
                (Ok(GpioState::Low), Ok(GpioState::High)) => "odd (up=low down=high)",
                _ => "read failed",
            };
            let _ = writeln!(Console::writer(), "  GP{p:<2}  {verdict}\r");
        }

        let _ = writeln!(
            Console::writer(),
            "  -- hold a button and wait for the next scan --\r"
        );
        let _ = Alarm::sleep_for(Milliseconds(4000));
    }
}
