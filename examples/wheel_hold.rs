//! Drive GP20 at one fixed frequency and never stop, so registers can be read.
//!
//! `wheel_probe` steps through frequencies with stopped phases between them,
//! so a register read at an uncontrolled moment can catch it idle and report
//! `CSR=0` for a perfectly healthy output. This holds the output on forever
//! and reports both the PWM driver's answer and what the counter hears, which
//! is what separates "the chip is not driving the pin" from "the pin is
//! driving and the wire is not carrying".
//!
//! PWM ch2 is GP20: CSR at 0x400a8028, EN alias at 0x400a80f0 bit 2.

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

const COUNTER: u32 = 0x00013;
const C_START: u32 = 1;
const C_RATE: u32 = 3;

const PWM: u32 = 0x00010;
const PWM_START: u32 = 1;

const WHEEL_PIN: u32 = 1;
const HZ: u32 = 1000;

fn main() {
    let mut console = Console::writer();

    let started = TockSyscalls::command(COUNTER, C_START, 0, 0).to_result::<(), ErrorCode>();
    let wheel = TockSyscalls::command(PWM, PWM_START, WHEEL_PIN | (5000 << 16), HZ)
        .to_result::<(), ErrorCode>();
    let _ = writeln!(
        console,
        "hold: counter start {started:?}, pwm start {wheel:?} at {HZ} Hz on pin {WHEEL_PIN}"
    );

    loop {
        let _ = Alarm::sleep_for(Milliseconds(1000));
        let pps = TockSyscalls::command(COUNTER, C_RATE, 0, 0)
            .to_result::<u32, ErrorCode>()
            .unwrap_or(0);
        let _ = writeln!(console, "hold: {HZ} Hz commanded -> {pps} pps");
    }
}
