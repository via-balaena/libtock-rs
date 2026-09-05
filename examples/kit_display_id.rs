//! Asks an unidentified TFT what controller it is.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_display_id
//! ```
//!
//! The kit's display carries no legible part number and is far too large for
//! either variant in `capsules/extra/src/st77xx.rs` — about a four-inch
//! diagonal, where `ST7735` is a 1.8in part and `ST7789H2` a 1.3in one. Panels
//! that size are ILI9341, ILI9486/9488 or ST7796, and Tock has a capsule for
//! none of them. Writing one blind costs a kernel rebuild per guess, so this
//! asks the panel instead.
//!
//! # Wiring this assumes
//!
//! SPI0 in SPI function on GP2 SCK, GP3 MOSI, GP4 MISO — the kit's own labels,
//! each landing on the matching SPI0 function in the RP2350 pin table. GP5 CS,
//! GP6 DC and GP7 RST are driven here as plain GPIO.
//!
//! **Chip select has to be ours.** A register read is one CS assertion spanning
//! a change of DC: CS low, DC low, command byte, DC high, read bytes, CS high.
//! `capsules/core/src/spi_controller.rs` asserts CS per `read_write_bytes` and
//! releases it on completion, exposes no hold, and answers `NOSUPPORT` to
//! command 3. Two syscalls would be two assertions, and the controller would
//! see the command terminated before its data phase. So the capsule's chip
//! select points at a spare pin and this drives the real one.
//!
//! # Reading the output
//!
//! All zeros or all `ff` means nothing is answering: MISO not connected, the
//! clock too fast for reads, or the wrong pins. That is why the rate is swept —
//! these controllers read far slower than they write, an ILI9341 to about
//! 6.6 MHz against writes past 10, and a bus left at a display-writing rate
//! fails reads while looking exactly like a dead data line.
//!
//! Anything else is an answer, and the table below names the ones worth
//! knowing. An unrecognised non-zero reply is still a result: it means the wires
//! are right and the panel is something else.

#![no_main]
#![no_std]

use core::fmt::Write;

use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio::{Gpio, OutputPin};
use libtock::runtime::{set_main, stack_size, TockSyscalls};
use libtock::spi_controller::SpiController;

set_main! {main}
stack_size! {0x800}

type Out<'a> = OutputPin<'a, TockSyscalls>;

const CS: u32 = 5;
const DC: u32 = 6;
const RST: u32 = 7;

/// Read commands worth trying, with the number of bytes to clock after each.
/// The first byte back is a dummy on every one of these.
const REGISTERS: [(u8, &str, usize); 5] = [
    (0xd3, "RDID4   ILI9341/9486/9488, ST7796", 4),
    (0x04, "RDDID   ST7735/ST7789", 4),
    (0x09, "RDDST   display status", 5),
    (0xda, "RDID1   manufacturer", 2),
    (0xdb, "RDID2   version", 2),
];

/// Rates to try, slowest first.
///
/// Upward rather than downward because the bus already starts where a first
/// read should: `VirtualSpiMasterDevice::new` defaults to 100 kHz, mode 0. An
/// answer at the bottom is a clean one, and climbing from it attributes any
/// later silence to speed. Starting fast and descending gets the same
/// information only if you are lucky about where it stops working.
const RATES: [u32; 4] = [200_000, 1_000_000, 4_000_000, 8_000_000];

/// The bench requires the bus to stay at or above this, so nothing below it is
/// ever requested.
///
/// The check is on the rate going in, not on the rate coming back: a request
/// below the floor has already been made by the time there is an answer to
/// inspect, so a guard on the answer could only fire in the case where nothing
/// was wrong. Every entry in `RATES` is above this today, which makes the check
/// a guard against a later edit rather than against the list as it stands —
/// which is the point of having it in the code rather than in a comment.
///
/// No diagnostic value is lost. Every controller this probe knows about reads
/// far faster than 100 kHz, so a rate below it could only make a working read
/// slower, never make a failing one work.
const MINIMUM_RATE_HZ: u32 = 100_000;

fn main() {
    let mut console = Console::writer();

    if SpiController::exists().is_err() {
        let _ = writeln!(
            console,
            "kit_display_id: no SPI controller driver on this kernel"
        );
        return;
    }

    let (mut cs_pin, mut dc_pin, mut rst_pin) =
        match (Gpio::get_pin(CS), Gpio::get_pin(DC), Gpio::get_pin(RST)) {
            (Ok(a), Ok(b), Ok(c)) => (a, b, c),
            _ => {
                let _ = writeln!(
                    console,
                    "kit_display_id: GP{CS}/GP{DC}/GP{RST} not available"
                );
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
            let _ = writeln!(console, "kit_display_id: could not drive the control pins");
            return;
        }
    };

    // Idle state before anything is asked of the panel.
    let _ = cs.set();
    let _ = dc.set();
    reset(&mut rst);

    for rate in RATES {
        if rate < MINIMUM_RATE_HZ {
            let _ = writeln!(
                console,
                "\r\n{rate} Hz is under the {MINIMUM_RATE_HZ} Hz floor, not requested\r"
            );
            continue;
        }

        if SpiController::set_baud_rate(rate).is_err() {
            let _ = writeln!(console, "\r\n{rate} Hz refused by the kernel\r");
            continue;
        }

        // What was asked for and what the divider produced are different
        // numbers, and the second one is the bus. Reported rather than acted
        // on: this is a correction to the label on the pass, so that a result
        // is filed under the rate that actually ran.
        let actual = SpiController::get_baud_rate().unwrap_or(0);
        let _ = writeln!(console, "\r\n=== asked {rate} Hz, bus at {actual} Hz ===\r");

        let mut answered = false;
        for (command, name, length) in REGISTERS {
            let mut buffer = [0u8; 8];
            let bytes = &mut buffer[..length];

            if read_register(&mut cs, &mut dc, command, bytes).is_err() {
                let _ = writeln!(console, "  {command:02x} {name}: transfer failed\r");
                continue;
            }

            let _ = write!(console, "  {command:02x} {name}:");
            for byte in bytes.iter() {
                let _ = write!(console, " {byte:02x}");
            }
            if let Some(part) = identify(command, bytes) {
                let _ = write!(console, "   <- {part}");
            }
            let _ = writeln!(console, "\r");

            if bytes.iter().any(|&byte| byte != 0x00 && byte != 0xff) {
                answered = true;
            }
        }

        if answered {
            let _ = writeln!(
                console,
                "  something answered at this rate -- the wires are right\r"
            );
        }
    }

    let _ = writeln!(
        console,
        "\r\nkit_display_id: done. All 00 or all ff at every rate means nothing \
         answered: MISO unconnected, wrong pins, or the panel is write-only.\r"
    );
}

/// Pulses reset, then waits out the controller's start-up.
fn reset(rst: &mut Out<'_>) {
    let _ = rst.set();
    let _ = Alarm::sleep_for(Milliseconds(20));
    let _ = rst.clear();
    let _ = Alarm::sleep_for(Milliseconds(20));
    let _ = rst.set();
    // ILI-family datasheets ask for 120 ms before the first command.
    let _ = Alarm::sleep_for(Milliseconds(150));
}

/// One command byte with DC low, then `out.len()` bytes clocked with DC high,
/// all inside one chip select.
fn read_register(
    cs: &mut Out<'_>,
    dc: &mut Out<'_>,
    command: u8,
    out: &mut [u8],
) -> Result<(), libtock_platform::ErrorCode> {
    let zeros = [0u8; 8];
    let length = out.len();

    let _ = cs.clear();
    let _ = dc.clear();
    let sent = SpiController::spi_controller_write_sync(&[command], 1);
    let _ = dc.set();
    let read = SpiController::spi_controller_write_read_sync(&zeros, out, length as u32);
    let _ = cs.set();

    sent.and(read)
}

/// Names a reply that matches a controller worth recognising. The first byte of
/// each of these is a dummy, so the match starts at index 1.
fn identify(command: u8, bytes: &[u8]) -> Option<&'static str> {
    match (command, bytes) {
        (0xd3, [_, 0x00, 0x93, 0x41, ..]) => Some("ILI9341, 320x240"),
        (0xd3, [_, 0x00, 0x94, 0x86, ..]) => Some("ILI9486, 480x320"),
        (0xd3, [_, 0x00, 0x94, 0x88, ..]) => Some("ILI9488, 480x320"),
        (0xd3, [_, 0x00, 0x77, 0x96, ..]) => Some("ST7796, 480x320"),
        (0x04, [_, 0x85, 0x85, 0x52, ..]) => Some("ST7789"),
        (0x04, [_, 0x5c, 0x89, ..]) => Some("ST7735"),
        _ => None,
    }
}
