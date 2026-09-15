//! Two PWM users at once, which `MuxPwm` used to make impossible.
//!
//! The mux kept a single `inflight` slot: the first user to start claimed it,
//! and while it was held every other user's operation was left in its cell and
//! never run -- while `PwmPinUser::start` went on answering `Ok(())`. One PWM
//! pin at a time, silently. Every board in the tree drives a buzzer, and a mux
//! with one user never contends, so nothing had noticed.
//!
//! This is the shape that found it: the throttle capsule owns GP19 and
//! re-asserts it every 20 ms, so it holds the slot for as long as it is open,
//! and the wheel this app drives on GP20 goes dead. GP20 reaches GP21 on the
//! bench jumper, so the pulse counter says whether the second output is
//! actually running rather than merely claimed.
//!
//! Needs no hands: an app owns its own throttle, so it can ask for one without
//! a pedal, and GP19 drives nothing on the bench.
//!
//! Build the board with `kit_input` and `wheel_source`.
//!
//! Expected, with the mux fixed:
//!
//! ```text
//!   throttle 0     -> actual 0     wheel 1000 Hz -> ~1016 pps
//!   throttle 5000  -> actual 5000  wheel 1000 Hz -> ~1016 pps
//!   throttle 10000 -> actual 10000 wheel 1000 Hz -> ~1020 pps
//! ```
//!
//! Before the fix the second and third lines read 0 pps, with the starved
//! channel's TOP, divider and compare all correctly programmed and `CSR.EN`
//! clear.

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

const PWM: u32 = 0x00010;
const PWM_START: u32 = 1;
const PWM_STOP: u32 = 2;

/// GP20 in the board's pwm pin list, jumpered to GP21.
const WHEEL_PIN: u32 = 1;
const WHEEL_HZ: u32 = 1000;
/// Half the counter's usable rate, so a starved output reads zero rather than
/// something that could be mistaken for jitter.
const TOLERANCE: u32 = WHEEL_HZ / 2;

fn main() {
    let mut console = Console::writer();

    let _ = TockSyscalls::command(COUNTER, C_START, 0, 0).to_result::<(), ErrorCode>();
    let _ = TockSyscalls::command(THROTTLE, T_ARM, 0, 0).to_result::<(), ErrorCode>();

    let mut failures = 0;
    for want in [0u32, 5000, 10000] {
        // The capsule slews at SCALE/20 per 20 ms tick, so full travel takes
        // about 400 ms; keep asking, because silence closes it.
        for _ in 0..20 {
            let _ = TockSyscalls::command(THROTTLE, T_SET, want, 0).to_result::<(), ErrorCode>();
            let _ = Alarm::sleep_for(Milliseconds(25));
        }
        let started = TockSyscalls::command(PWM, PWM_START, WHEEL_PIN | (5000 << 16), WHEEL_HZ)
            .to_result::<(), ErrorCode>();
        // Two full counter windows.
        for _ in 0..24 {
            let _ = TockSyscalls::command(THROTTLE, T_SET, want, 0).to_result::<(), ErrorCode>();
            let _ = Alarm::sleep_for(Milliseconds(25));
        }
        let actual = TockSyscalls::command(THROTTLE, T_ACTUAL, 0, 0)
            .to_result::<u32, ErrorCode>()
            .unwrap_or(0);
        let pps = TockSyscalls::command(COUNTER, C_RATE, 0, 0)
            .to_result::<u32, ErrorCode>()
            .unwrap_or(0);
        let ok = started.is_ok() && pps.abs_diff(WHEEL_HZ) <= TOLERANCE;
        if !ok {
            failures += 1;
        }
        let _ = writeln!(
            console,
            "contend: throttle {want} -> actual {actual}, wheel {WHEEL_HZ} Hz -> {pps} pps  {}",
            if ok { "ok" } else { "STARVED" }
        );
        let _ = TockSyscalls::command(PWM, PWM_STOP, WHEEL_PIN, 0).to_result::<(), ErrorCode>();
    }
    let _ = TockSyscalls::command(THROTTLE, T_SET, 0, 0).to_result::<(), ErrorCode>();
    let _ = writeln!(
        console,
        "contend: {}",
        if failures == 0 {
            "both PWM users ran together"
        } else {
            "a second PWM user was starved by the mux"
        }
    );
}
