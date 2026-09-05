//! Reproduction for the UART mux receive teardown.
//!
//! On a board that pairs a ProcessConsole with the userspace console capsule on
//! one `UartMux`, over a chip whose UART driver invokes the receive callback
//! before clearing `rx_status`, an application's first console read is accepted
//! and then killed by the kernel's own restart. The application sees the read
//! complete with zero bytes and `BUSY`.
//!
//! Run with:
//!
//! ```text
//! make qemu-example EXAMPLE=console_read_busy
//! ```
//!
//! Expected on an affected build: `read -> 0 bytes, Err(Busy)`.
//! If the read instead blocks forever, the teardown did not happen.
//!
//! Deliberately blocking rather than async: the path under test is the console
//! capsule and the mux, so there is no reason to make a reader take an executor
//! on trust as well.
//!
//! The sleep is required, not cosmetic. `ProcessConsole::start()` does not arm a
//! receive — it sets a 100 ms alarm and arms in the callback. A read issued
//! before that fires finds an idle mux, takes the ordinary path and stays
//! outstanding, which looks exactly like the defect being absent.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut console = Console::writer();
    let _ = writeln!(
        console,
        "console_read_busy: waiting out the process console's startup alarm"
    );

    // Must exceed ProcessConsole's 100 ms startup alarm, or the precondition
    // for the defect does not exist yet and a clean read proves nothing.
    if Alarm::sleep_for(Milliseconds(500)).is_err() {
        let _ = writeln!(console, "no alarm driver, cannot establish precondition");
        return;
    }

    let _ = writeln!(console, "issuing one console read");

    let mut buffer = [0u8; 16];
    let (count, result) = Console::read(&mut buffer);

    let _ = writeln!(console, "read -> {} bytes, {:?}", count, result);
}
