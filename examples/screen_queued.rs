//! App B of a two-app test: the one whose queued command must survive.
//!
//! Issues exactly one screen call during the panel's init window, when the
//! driver answers BUSY. Post tock `5c0185624` the capsule leaves that command
//! queued and serves it from `screen_is_ready`, so this should report `Ok`
//! after roughly 1,240 ms.
//!
//! The point of the pair is what a SECOND app can do to that queued command.
//! See `screen_jostle.rs`, which runs in slot 2 and walks the queue from
//! inside its own downcall. Before tock `71e592db6` this app reported
//! `Err(BUSY)` early instead, because the dequeue path treated BUSY as a
//! failure and dropped the command.
//!
//! Build for `raspberry_pi_pico_2_w_slot1`, with the board's `kit_display`.

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
    let hz = Alarm::get_frequency().map(|f| f.0).unwrap_or(1);
    let t0 = Alarm::get_ticks().unwrap_or(0);

    // Ordering matters and cannot be assumed: this test only exercises the
    // dequeue path if THIS app's command is already queued when the other
    // app calls fill. Print the moment of issue so the interleaving is
    // visible rather than inferred.
    let _ = writeln!(console, "B: issuing set_write_frame at t={t0}");
    let frame = Screen::set_write_frame(0, 0, 480, 320);

    let t1 = Alarm::get_ticks().unwrap_or(0);
    let ms = t1.wrapping_sub(t0) as u64 * 1000 / hz.max(1) as u64;
    let _ = writeln!(console, "B: set_write_frame {frame:?} after {ms} ms");
    let _ = writeln!(
        console,
        "B: {}",
        match frame {
            Ok(()) => "queued command SURVIVED the other app's downcall",
            Err(_) => "queued command was LOST -- dequeued into a busy driver",
        }
    );
}
