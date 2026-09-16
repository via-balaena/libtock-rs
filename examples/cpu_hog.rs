//! Burns CPU forever, making no syscalls, so something else has to be
//! preempted to run.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=cpu_hog
//! ```
//!
//! The other half of the scheduler-jitter measurement: `jitter_probe` alone is
//! a baseline that cannot fail, and a baseline that cannot fail proves nothing
//! about whether the probe can see jitter at all. This supplies the contention
//! that makes the probe's clean run meaningful.
//!
//! # No syscall in the loop, and that is load-bearing
//!
//! Any syscall may yield, and a hog that yields is not a hog — it hands the
//! scheduler exactly the opportunity the measurement is trying to deny it. So
//! there is one `writeln!` before the loop and nothing inside it: no console,
//! no alarm, no `yield`. The app is deliberately unkillable by its own logic
//! and is stopped by resetting the board.
//!
//! # Why the arithmetic cannot be optimised away
//!
//! An empty `loop {}` is a legal infinite loop that LLVM will happily reduce to
//! a branch-to-self, which still burns the CPU — but a loop whose *result* is
//! unused can be deleted outright, and then the app would sit in a tight branch
//! doing nothing measurable or, worse, be reshaped in a way that changes how it
//! interacts with interrupts. `black_box` makes the accumulator observable to
//! the compiler, so the multiply-add chain is really executed.
//!
//! Stable since 1.66; this crate's MSRV is 1.88.

#![no_main]
#![no_std]

use core::fmt::Write;
use core::hint::black_box;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

fn main() {
    let mut console = Console::writer();
    let _ = writeln!(
        console,
        "cpu_hog: spinning forever, no syscalls from here on. Reset the board to stop it.\r"
    );

    // A dependent chain: each iteration needs the previous result, so this
    // cannot be vectorised or hoisted into a constant.
    let mut acc: u32 = 1;
    loop {
        acc = black_box(acc)
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        black_box(acc);
    }
}
