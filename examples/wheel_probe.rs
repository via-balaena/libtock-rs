//! Check the pulse counter against a pulse train whose rate we chose.
//!
//! A wheel sensor you can only observe tells you the counter produced a
//! number. A pulse train at a frequency this board set tells you whether the
//! number is right, which is the difference between testing and watching.
//!
//! Needs the bench jumper that is already fitted, GP20 to GP21: PWM index 1
//! drives GP20, the counter's channel 0 counts GP21. Channel 1 is GP22, which
//! is pulled down and connected to nothing, so it is the negative control --
//! a counter that reports road speed on an unconnected input is worse than one
//! that reports none.
//!
//! One rising edge per period, so the expected rate in pulses per second is
//! simply the frequency. Duty cycle should not change the count and 50% is
//! only a convenient value.

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

const PWM: u32 = 0x00010;
const PWM_START: u32 = 1;
const PWM_STOP: u32 = 2;
/// PWM index 1 is GP20.
const SOURCE_PIN: u32 = 1;
const DUTY: u32 = 5000;

const COUNTER: u32 = 0x00013;
const CNT_START: u32 = 1;
const CNT_STOP: u32 = 2;
const CNT_RATE: u32 = 3;
const CNT_CHANNELS: u32 = 4;
const CNT_WINDOW: u32 = 5;

fn cmd(driver: u32, num: u32, a: u32, b: u32) -> Result<u32, ErrorCode> {
    TockSyscalls::command(driver, num, a, b).to_result::<u32, ErrorCode>()
}

fn main() {
    let mut console = Console::writer();

    let channels = cmd(COUNTER, CNT_CHANNELS, 0, 0).unwrap_or(0);
    let window = cmd(COUNTER, CNT_WINDOW, 0, 0).unwrap_or(0);
    if channels == 0 {
        let _ = writeln!(console, "wheel: no pulse counter on this board");
        return;
    }
    let _ = writeln!(
        console,
        "wheel: {channels} channels, {window} ms window; ch0 = GP21 (jumpered), ch1 = GP22 (open)"
    );

    let _ = TockSyscalls::command(COUNTER, CNT_START, 0, 0).to_result::<(), ErrorCode>();

    for hz in [50u32, 137, 400, 1000] {
        let packed = SOURCE_PIN | (DUTY << 16);
        if let Err(e) =
            TockSyscalls::command(PWM, PWM_START, packed, hz).to_result::<(), ErrorCode>()
        {
            let _ = writeln!(console, "wheel: could not drive the source at {hz} Hz: {e:?}");
            continue;
        }

        // Longer than one window, so the reading is of a window that was
        // entirely at this frequency rather than one that spans the change.
        let _ = Alarm::sleep_for(Milliseconds(700));

        let measured = cmd(COUNTER, CNT_RATE, 0, 0).unwrap_or(0);
        let open = cmd(COUNTER, CNT_RATE, 1, 0).unwrap_or(0);
        let err = if hz > 0 {
            (measured as i64 - hz as i64) * 100 / hz as i64
        } else {
            0
        };
        let _ = writeln!(
            console,
            "wheel: source {hz:5} Hz -> ch0 {measured:5} pps ({err:+}%), ch1 {open} pps"
        );
    }

    let _ = TockSyscalls::command(PWM, PWM_STOP, SOURCE_PIN, 0).to_result::<(), ErrorCode>();
    let _ = Alarm::sleep_for(Milliseconds(700));
    let stopped = cmd(COUNTER, CNT_RATE, 0, 0).unwrap_or(0);
    let _ = writeln!(console, "wheel: source stopped -> ch0 {stopped} pps");

    let _ = TockSyscalls::command(COUNTER, CNT_STOP, 0, 0).to_result::<(), ErrorCode>();
}
