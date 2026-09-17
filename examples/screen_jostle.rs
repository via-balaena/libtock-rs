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
//! The first working version depended on the console write below: console is
//! asynchronous, so printing yields, and B got to issue and queue its command
//! before this app resumed into `fill`. That is an instrument-induced yield
//! doing load-bearing work -- correct, and invisible to anyone "tidying up" the
//! print.
//!
//! **So the wait is explicit now, and it is bounded at BOTH ends.** Too short
//! and B has not queued yet, which is the original failure: a pass that proves
//! nothing. Too long and the panel has finished initialising, so there is no
//! busy driver to dequeue into and the test passes for the other wrong reason.
//! The window is roughly 1 ms at the bottom and the init gap -- 1,240 ms
//! measured -- at the top. 50 ms sits two orders of magnitude clear of one end
//! and twenty-five times clear of the other.
//!
//! The print stays, because the transcript is the evidence. It is no longer
//! what makes the test work.
//!
//! So a PASS here is only meaningful alongside the console transcript showing
//! both apps issuing before either completes. Verified both ways on a Pico 2 W:
//! against tock 5c0185624 B reports `Err(BUSY)` after 15 ms and says LOST;
//! against 71e592db6 it reports `Ok(())` after 1238 ms and says SURVIVED, with
//! B issuing at t=2757 us and A at t=52656 us.
//!
//! # That transcript rule is sufficient, and it was tested
//!
//! A 2,000 ms settle was run against the BROKEN kernel and reported SURVIVED --
//! inert, exactly as the upper bound predicts. **What distinguishes it is
//! visible in the transcript and nowhere else: B's verdict line prints BEFORE
//! A's issue line, which cannot happen in a valid run.** So the check is a
//! concrete predicate, not a disposition to be careful:
//!
//! ```text
//! valid : B issues  ->  A issues  ->  B reports
//! inert : B issues  ->  B reports ->  A issues     (SURVIVED means nothing)
//! ```
//!
//! The timestamps on every line make that orderable by reading, not inferring.
//!
//! Build for `raspberry_pi_pico_2_w_slot2`, with the board's `kit_display`.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut console = Console::writer();

    // Let B issue and queue its command first. Bounded at both ends -- see the
    // module docs -- and the upper bound is the dangerous one, so it is a build
    // error rather than a comment.
    const SETTLE_MS: u32 = 50;
    /// Long enough for B to reach the capsule. Measured: B issues at ~2.8 ms.
    ///
    /// `allow(dead_code)` because rustc does not count a use inside an
    /// anonymous `const _`, and both of these are used only by the assertions
    /// below. The guard is real -- setting `SETTLE_MS` to 2000 fails the build
    /// with "settle is too long" -- so deleting these to satisfy the lint
    /// would delete a working compile-time check.
    #[allow(dead_code)]
    const MIN_SETTLE_MS: u32 = 1;
    /// The panel's init gap. Past this the driver is idle, there is nothing to
    /// dequeue into, and the run is inert -- it reports SURVIVED on a kernel
    /// known to be broken. Measured at 1,239 ms on the kit's ST7796.
    #[allow(dead_code)]
    const INIT_GAP_MS: u32 = 1_239;
    const _: () = assert!(
        SETTLE_MS >= MIN_SETTLE_MS,
        "settle is too short: B will not have queued"
    );
    const _: () = assert!(
        SETTLE_MS < INIT_GAP_MS,
        "settle is too long: the driver will be idle and the run inert"
    );

    let _ = Alarm::sleep_for(Milliseconds(SETTLE_MS));

    // No set_write_frame anywhere above this line -- that is the whole point.
    let t0 = Alarm::get_ticks().unwrap_or(0);
    let _ = writeln!(
        console,
        "A: issuing fill at t={t0} (after {SETTLE_MS} ms settle)"
    );
    let mut buf = [0u8; 2];
    let filled = Screen::fill(&mut buf, 0x07E0);
    let _ = writeln!(console, "A: fill with no write frame -> {filled:?}");
}
