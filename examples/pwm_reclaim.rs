//! Ask for a PWM pin whose previous owner died holding it.
//!
//! The other half of the check; see `pwm_owner`, which takes pin 0 and
//! terminates without releasing it.
//!
//! What the two outcomes mean:
//!
//! * `CLAIMED` -- the capsule noticed the recorded owner no longer exists,
//!   stopped the output and handed the pin over. This is the fix working.
//! * `REFUSED Reserve` -- the stale `ProcessId` is still being compared for
//!   equality, so the pin is stranded for the life of the boot and still
//!   driven at the dead process's duty cycle.
//!
//! The wait is what makes the order deterministic: both processes start at
//! boot, and this one must ask after the other has finished dying.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::{ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x200}

/// `capsules_core::driver::NUM::Pwm`.
const DRIVER_NUM: u32 = 0x00010;
const CMD_START: u32 = 1;

const DUTY: u32 = 2500;
const FREQ_HZ: u32 = 1000;
const PIN: u32 = 0;

fn main() {
    let mut console = Console::writer();
    let _ = Alarm::sleep_for(Milliseconds(3000));

    let packed = PIN | (DUTY << 16);
    match TockSyscalls::command(DRIVER_NUM, CMD_START, packed, FREQ_HZ)
        .to_result::<(), ErrorCode>()
    {
        Ok(()) => {
            let _ = writeln!(
                console,
                "pwm-reclaim: CLAIMED pin {PIN} -- a dead owner no longer strands it"
            );
        }
        Err(e) => {
            let _ = writeln!(
                console,
                "pwm-reclaim: REFUSED pin {PIN} with {e:?} -- still stranded"
            );
        }
    }
}
