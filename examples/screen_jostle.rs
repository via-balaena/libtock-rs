//! App A of a two-app test: the one that walks the queue from a downcall.
//!
//! Calls `fill` having never set a write frame. `app.width` and `app.height`
//! are written only by the SetWriteFrame arm, so they are zero here, and the
//! capsule's Fill arm takes its zero-length path -- which calls
//! `run_next_command` from inside `call_screen`, with the driver's state
//! neither idle nor checked (screen/screen.rs:282).
//!
//! That walk can dequeue a DIFFERENT app's queued command into a panel that
//! is still initialising. See `screen_queued.rs`, which is that app: it is
//! the one whose result matters, not this one.
//!
//! # THIS TEST DEPENDS ON AN ORDERING IT DOES NOT CONTROL
//!
//! It only exercises the dequeue path if app B's command is ALREADY queued
//! when this app calls `fill`. There is no IPC here to arrange that, and the
//! first version of this pair had no `writeln!` before the fill -- so this
//! app reached the capsule first, nothing was queued, and **the test passed
//! against a kernel that was demonstrably broken.**
//!
//! What makes it work is the console write below: console is asynchronous, so
//! printing yields, and B gets to issue and queue its command before this app
//! resumes into `fill`. That is an instrument-induced yield doing load-bearing
//! work, which is worth knowing before anyone "tidies up" the print.
//!
//! So a PASS here is only meaningful alongside the console transcript showing
//! both apps issuing before either completes. Verified both ways on a Pico 2 W:
//! against tock 5c0185624 B reports `Err(BUSY)` after 15 ms and says LOST;
//! against 71e592db6 it reports `Ok(())` after 1238 ms and says SURVIVED.
//!
//! Build for `raspberry_pi_pico_2_w_slot2`, with the board's `kit_display`.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut console = Console::writer();

    // No set_write_frame anywhere above this line -- that is the whole point.
    let t0 = Alarm::get_ticks().unwrap_or(0);
    let _ = writeln!(console, "A: issuing fill at t={t0}");
    let mut buf = [0u8; 2];
    let filled = Screen::fill(&mut buf, 0x07E0);
    let _ = writeln!(console, "A: fill with no write frame -> {filled:?}");
}
