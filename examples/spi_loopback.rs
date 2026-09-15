//! SPI loopback test.
//!
//! Writes a varied pattern and reads simultaneously. With MOSI jumpered to
//! MISO, what comes back must be byte-identical to what went out. That tests
//! the SPI driver alone -- clock divider, framing, interrupt handling, the
//! transfer path -- with nothing else in the circuit that can be wrong.
//!
//! On a Pico 2 with SPI0 on the GP4-GP7 group:
//!     jumper GP7 (TX / MOSI, header pin 10) to GP4 (RX / MISO, header pin 6)
//!
//! Take the board out of any accessory kit first. Every other SPI0 pin group
//! collides with something: GP0 is the console, GP16/GP17 are often LEDs whose
//! series resistors fight the input, and GP23 is the CYW43 on a Pico 2 W.
//!
//! Repeats, so a capture started at any moment sees a result.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock::spi_controller::SpiController;

set_main! {main}
stack_size! {0x800}

const N: usize = 32;

fn main() {
    // A varied pattern, not a constant: a stuck line would pass a run of 0x12.
    let mut tx = [0u8; N];
    let mut i = 0;
    while i < N {
        tx[i] = (i as u8).wrapping_mul(7).wrapping_add(0x5a);
        i += 1;
    }

    let _ = writeln!(
        Console::writer(),
        "\r\nspi loopback: jumper GP7 (TX) to GP4 (RX)\r"
    );

    if SpiController::exists().is_err() {
        let _ = writeln!(
            Console::writer(),
            "  no SPI controller driver on this board\r"
        );
        return;
    }

    let mut round: u32 = 0;
    loop {
        round += 1;
        let mut rx = [0u8; N];

        match SpiController::spi_controller_write_read_sync(&tx, &mut rx, N as u32) {
            Ok(()) => {
                if rx == tx {
                    let _ = writeln!(
                        Console::writer(),
                        "  round {round}: PASS -- {N} bytes byte-identical\r"
                    );
                } else {
                    let mut diff = 0usize;
                    let mut j = 0;
                    while j < N {
                        if rx[j] != tx[j] {
                            diff += 1;
                        }
                        j += 1;
                    }
                    let _ = writeln!(
                        Console::writer(),
                        "  round {round}: MISMATCH -- {diff} of {N} bytes differ\r"
                    );
                    let _ = writeln!(Console::writer(), "    sent {:02x?}\r", &tx[..8]);
                    let _ = writeln!(Console::writer(), "    got  {:02x?}\r", &rx[..8]);
                }
            }
            Err(e) => {
                let _ = writeln!(Console::writer(), "  round {round}: error {e:?}\r");
            }
        }

        let _ = Alarm::sleep_for(Milliseconds(2000));
    }
}
