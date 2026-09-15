//! BENCH INSTRUMENT. Claims GP16 through the reclaim-leak demo driver, turns
//! the LED on, and exits cleanly.
//!
//! The point is what happens next: this process is gone, but the pin it
//! claimed stays driven and stays owned. See `leak_observe.rs`.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::Syscalls;
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x400}

const DRIVER_NUM: u32 = 0xF0001;
const CLAIM: u32 = 1;
const PIN: u32 = 0; // index into the driver's pin array; GP16 on this board

fn main() {
    let mut c = Console::writer();
    if TockSyscalls::command(DRIVER_NUM, 0, 0, 0).to_result::<(), libtock::platform::ErrorCode>()
        .is_err()
    {
        writeln!(c, "[A] leak-demo driver unavailable").unwrap();
        return;
    }

    match TockSyscalls::command(DRIVER_NUM, CLAIM, PIN, 0)
        .to_result::<(), libtock::platform::ErrorCode>()
    {
        Ok(()) => writeln!(c, "[A] claimed GP16, LED on. Exiting now.").unwrap(),
        Err(e) => writeln!(c, "[A] claim failed: {e:?}").unwrap(),
    }

    // Return from main == clean exit-terminate. The process is gone; the LED
    // is not.
}
