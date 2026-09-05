//! Turns a stepper motor one revolution forward, then back.
//!
//! Sized for a 28BYJ-48 in half-step mode: 4096 half-steps per output
//! revolution, 2 ms apart. The board decides which pins the motor is on; an
//! application only asks for movement.
//!
//! The interesting part is what this example does *not* have to do. Driving the
//! same motor through raw GPIO means owning the phase sequence and the timing,
//! and it means a fault mid-step leaves a coil energised — nothing in Tock
//! releases a GPIO pin when a process dies, so the motor stays hot for as long
//! as the board is powered. Here the kernel owns the coils, checks on each step
//! whether this process is still alive, and de-energises if it is not. The worst
//! case is one step interval rather than indefinitely.
//!
//! That is also why the syscall round trip does not set the step rate: it is
//! paid once per movement rather than once per step.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock::stepper::{Interval, Stepper};

set_main! {main}
stack_size! {0x400}

/// A 28BYJ-48 is 64 steps per internal revolution behind a 64:1 gearbox, so one
/// output revolution is 4096 half-steps.
const HALF_STEPS_PER_REVOLUTION: u32 = 4096;

/// 2 ms between steps. The capsule owns the timing, so this is what the motor
/// gets rather than what the syscall layer can manage.
const INTERVAL: Interval = Interval(2000);

fn main() {
    let mut console = Console::writer();

    if Stepper::exists().is_err() {
        let _ = writeln!(console, "stepper: no stepper capsule on this board");
        return;
    }

    for (label, result) in [
        (
            "forward",
            Stepper::step_forward(HALF_STEPS_PER_REVOLUTION, INTERVAL),
        ),
        (
            "reverse",
            Stepper::step_reverse(HALF_STEPS_PER_REVOLUTION, INTERVAL),
        ),
    ] {
        match result {
            // Fewer steps than asked for is a success, not a failure: the
            // movement was stopped, and the count is how the caller knows where
            // the motor ended up.
            Ok(taken) => {
                let _ = writeln!(console, "stepper: {} {} steps", label, taken);
            }
            Err(error) => {
                let _ = writeln!(console, "stepper: {} failed: {:?}", label, error);
                return;
            }
        }
    }

    let _ = writeln!(console, "stepper: done");
}
