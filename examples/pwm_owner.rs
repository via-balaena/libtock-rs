//! Claim a PWM pin, start it, and die while still holding it.
//!
//! Half of a two-process check on `capsules_extra::pwm`. This one takes pin 0,
//! starts it, and returns from `main` -- which is a real termination:
//! `Termination for ()` calls `exit_terminate(0)`. It deliberately does NOT
//! send the Stop command, because a process that crashes never gets to.
//!
//! Run it alongside `pwm_reclaim`, which asks for the same pin afterwards.
//! Before the capsule learned to check whether the owner still exists, the
//! `ProcessId` left in `active_process` refused every later claimant with
//! `RESERVE` -- permanently, since a restarted process is given a new
//! identifier. The pin also kept running at this duty cycle.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::{ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x200}

/// `capsules_core::driver::NUM::Pwm`.
const DRIVER_NUM: u32 = 0x00010;
const CMD_START: u32 = 1;
const CMD_PIN_COUNT: u32 = 4;

/// Duty cycle is a four digit number, so 5000 is 50%.
const DUTY: u32 = 5000;
const FREQ_HZ: u32 = 1000;
const PIN: u32 = 0;

fn main() {
    let mut console = Console::writer();

    let pins = TockSyscalls::command(DRIVER_NUM, CMD_PIN_COUNT, 0, 0).to_result::<u32, ErrorCode>();
    let _ = writeln!(console, "pwm-owner: driver reports {pins:?} pins");

    // The capsule packs the pin and the duty cycle into one argument.
    let packed = PIN | (DUTY << 16);
    let started =
        TockSyscalls::command(DRIVER_NUM, CMD_START, packed, FREQ_HZ).to_result::<(), ErrorCode>();
    let _ = writeln!(console, "pwm-owner: start pin {PIN} -> {started:?}");

    let _ = writeln!(
        console,
        "pwm-owner: terminating WITHOUT stopping the pin -- this is the leak"
    );
}
