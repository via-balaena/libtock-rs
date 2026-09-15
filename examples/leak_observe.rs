//! BENCH INSTRUMENT. Runs after `leak_claim` has exited, and narrates what
//! `capsules/extra/src/pwm.rs`'s ownership logic does with a dead owner.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::Syscalls;
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x400}

const DRIVER_NUM: u32 = 0xF0001;
const CLAIM: u32 = 1;
const QUERY: u32 = 3;
const RECLAIM: u32 = 4;
const PIN: u32 = 0;

type E = libtock::platform::ErrorCode;

fn query<W: Write>(c: &mut W, label: &str) {
    match TockSyscalls::command(DRIVER_NUM, QUERY, PIN, 0).to_result::<u32, E>() {
        Ok(0) => writeln!(c, "[B] {label}: pin is UNCLAIMED").unwrap(),
        Ok(1) => writeln!(c, "[B] {label}: pin is claimed BY ME").unwrap(),
        Ok(_) => writeln!(c, "[B] {label}: pin is claimed by ANOTHER process").unwrap(),
        Err(e) => writeln!(c, "[B] {label}: query failed {e:?}").unwrap(),
    }
}

fn try_claim<W: Write>(c: &mut W, label: &str) {
    match TockSyscalls::command(DRIVER_NUM, CLAIM, PIN, 0).to_result::<(), E>() {
        Ok(()) => writeln!(c, "[B] {label}: claim SUCCEEDED").unwrap(),
        Err(e) => writeln!(c, "[B] {label}: claim REFUSED, {e:?}").unwrap(),
    }
}

fn main() {
    let mut c = Console::writer();
    Alarm::sleep_for(Milliseconds(4000)).unwrap();

    writeln!(c, "").unwrap();
    writeln!(c, "[B] --- process A has exited; LED should still be lit ---").unwrap();
    query(&mut c, "before");
    try_claim(&mut c, "before");

    writeln!(c, "[B] running the adc.rs liveness check pwm.rs lacks...").unwrap();
    match TockSyscalls::command(DRIVER_NUM, RECLAIM, PIN, 0).to_result::<u32, E>() {
        Ok(1) => writeln!(c, "[B] dead owner found and reclaimed; LED should be OFF").unwrap(),
        Ok(_) => writeln!(c, "[B] owner still live, nothing reclaimed").unwrap(),
        Err(e) => writeln!(c, "[B] reclaim failed {e:?}").unwrap(),
    }

    query(&mut c, "after");
    try_claim(&mut c, "after");
    writeln!(c, "[B] done. LED lit again means the pin was recoverable.").unwrap();
}
