//! Prove the throttle reaches the PIN, not just the PWM slice.
//!
//! Reading the slice's enable bit says the capsule drove the peripheral. It
//! says nothing about whether the pad is pointed at that peripheral, and a pad
//! left unmuxed reads exactly like a working one from the kernel's side --
//! which is what happened here first time round.
//!
//! So this measures the output from outside: build the board with
//! `throttle_on_jumper`, which puts the throttle on GP20, and the existing
//! bench jumper carries it to GP21 where the pulse counter is watching.
//!
//! The silence timeout is what makes this observable from one process. Death
//! closes the throttle too, but the process that died cannot report what
//! happened afterwards; going quiet closes it just as thoroughly while this
//! app is still alive to watch the count fall.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::{ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x400}

const THROTTLE: u32 = 0x00012;
const T_ARM: u32 = 1;
const T_SET: u32 = 2;
const T_ACTUAL: u32 = 4;

const COUNTER: u32 = 0x00013;
const C_START: u32 = 1;
const C_RATE: u32 = 3;

const SCALE: u32 = 10_000;

fn rate() -> u32 {
    TockSyscalls::command(COUNTER, C_RATE, 0, 0)
        .to_result::<u32, ErrorCode>()
        .unwrap_or(0)
}

fn actual() -> u32 {
    TockSyscalls::command(THROTTLE, T_ACTUAL, 0, 0)
        .to_result::<u32, ErrorCode>()
        .unwrap_or(0)
}

fn main() {
    let mut console = Console::writer();

    let _ = TockSyscalls::command(COUNTER, C_START, 0, 0).to_result::<(), ErrorCode>();
    if TockSyscalls::command(THROTTLE, T_ARM, 0, 0)
        .to_result::<(), ErrorCode>()
        .is_err()
    {
        let _ = writeln!(console, "wire: could not arm");
        return;
    }

    let _ = writeln!(console, "wire: open the throttle and watch GP21");
    for _ in 0..12 {
        let _ = TockSyscalls::command(THROTTLE, T_SET, SCALE, 0).to_result::<(), ErrorCode>();
        let _ = Alarm::sleep_for(Milliseconds(100));
    }
    let _ = writeln!(
        console,
        "wire: throttle {} -> counter {} pps",
        actual(),
        rate()
    );

    let _ = writeln!(console, "wire: going silent, the capsule should close it");
    for _ in 0..12 {
        let _ = Alarm::sleep_for(Milliseconds(100));
    }
    let _ = writeln!(
        console,
        "wire: throttle {} -> counter {} pps",
        actual(),
        rate()
    );
}
