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
//! About the sleep. `ProcessConsole::start()` does not arm a receive — it sets a
//! 100 ms alarm and arms in the callback. In principle a read issued before that
//! fires finds an idle mux, takes the ordinary path and stays outstanding, which
//! would look exactly like the defect being absent.
//!
//! In practice that window is not reachable from an application. Removing the
//! sleep, and even issuing the read before any `writeln!` so that no kernel
//! round trip precedes it, still yields `Err(BUSY)` under QEMU, with the
//! process console's `tock$` prompt appearing first. The process does not get
//! its first instruction until after the kernel's alarm has fired. So the sleep
//! is not what creates the failure — it only makes the ordering legible.
//!
//! This matters for reading the result: there is no window in which an
//! application can avoid the trigger, so "the app read too early" is not an
//! available explanation for the BUSY.
//!
//! A real control needs a kernel without a ProcessConsole on that mux, which is
//! a board change rather than an application one. Absent that, this example
//! demonstrates the defect but cannot demonstrate its own ability to fail — a
//! limitation worth stating rather than papering over.

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
