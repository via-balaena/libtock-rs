//! One `set_write_frame`, no retry: what comes back, and when.
//!
//! The kit's ST7796 answers BUSY to any frame call until its init sequence
//! finishes, about 1.25 s after boot. `Screen::exists()` and
//! `get_resolution()` are answered by the capsule without touching the driver,
//! so they succeed immediately and tell an app nothing.
//!
//! The capsule already has the mechanism that should make this a non-problem:
//! `hil::screen::ScreenClient::screen_is_ready` (kernel/src/hil/screen.rs:372)
//! is raised by st77xx.rs:541 and handled at screen/screen.rs:474, where it
//! calls `run_next_command` -- which walks every app and starts the first one
//! with `pending_command` set. A command that stayed queued would simply run
//! at ready.
//!
//! So this app is the measurement, not a workaround. Exactly one call, and it
//! reports the answer and the elapsed time:
//!
//! * BUSY at ~0 ms  -> the command was discarded; an app must retry.
//! * Ok at ~1250 ms -> it was queued and the ready path ran it.
//!
//! Observed 2026-09-16, four bench runs across two kernels: always the second,
//! never the first. See `screen_first_draw.rs`, which retries and reports the
//! raw codes rather than translating them.
//!
//! Build the board with `kit_display`.

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

    if Screen::exists().is_err() {
        let _ = writeln!(console, "ready: no screen -- build with kit_display");
        return;
    }
    let (w, h) = Screen::get_resolution().unwrap_or((480, 320));
    let hz = Alarm::get_frequency().map(|f| f.0).unwrap_or(1);
    let t0 = Alarm::get_ticks().unwrap_or(0);

    // The whole experiment: one call, no loop, no sleep.
    let frame = Screen::set_write_frame(0, 0, w, h);

    let t1 = Alarm::get_ticks().unwrap_or(0);
    let ms = t1.wrapping_sub(t0) as u64 * 1000 / hz.max(1) as u64;
    let _ = writeln!(
        console,
        "ready: {w}x{h} set_write_frame {frame:?} after {ms} ms"
    );

    // If it did come back Ok, the panel should take a fill as well.
    if frame.is_ok() {
        let mut buf = [0u8; 2];
        let t2 = Alarm::get_ticks().unwrap_or(0);
        let fill = Screen::fill(&mut buf, 0x07E0);
        let t3 = Alarm::get_ticks().unwrap_or(0);
        let _ = writeln!(
            console,
            "ready: fill {fill:?} in {} ms",
            t3.wrapping_sub(t2) as u64 * 1000 / hz.max(1) as u64
        );
    }
}
