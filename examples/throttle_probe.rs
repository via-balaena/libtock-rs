//! Exercise every way `capsules_extra::throttle` is supposed to close.
//!
//! Hands off on purpose. The pedal app proves the ECU works when someone is
//! driving it; this proves the capsule holds when nobody is, which is the half
//! that matters and the half a joystick cannot demonstrate repeatably.
//!
//! Four phases, each printing what the output is actually at, which the
//! capsule reports separately from what was asked for:
//!
//! 1. **Slew.** Ask for full immediately. The output must not jump: it should
//!    climb in steps and take about 400 ms to arrive.
//! 2. **Silence.** Stop asking, while still reading. Reading is deliberately
//!    not a keepalive, so after about 200 ms the capsule should force the
//!    target to zero and the output should fall at the same limited rate.
//! 3. **Recovery.** Ask for full again, to show that the timeout closed the
//!    throttle rather than latching it off.
//! 4. **Death.** Return from `main`, which terminates the process while the
//!    throttle is wide open and never sends a Stop. The capsule should notice
//!    on its next tick and close it. Read the PWM slice over SWD afterwards to
//!    see that it did.

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
const CMD_ARM: u32 = 1;
const CMD_SET: u32 = 2;
const CMD_ACTUAL: u32 = 4;
const SCALE: u32 = 10_000;

fn actual() -> u32 {
    TockSyscalls::command(THROTTLE, CMD_ACTUAL, 0, 0)
        .to_result::<u32, ErrorCode>()
        .unwrap_or(u32::MAX)
}

fn set(target: u32) {
    let _ = TockSyscalls::command(THROTTLE, CMD_SET, target, 0).to_result::<(), ErrorCode>();
}

fn watch(console: &mut impl Write, label: &str, ms: u32, keep_asking: Option<u32>) {
    let steps = ms / 50;
    for _ in 0..steps {
        if let Some(t) = keep_asking {
            set(t);
        }
        let _ = Alarm::sleep_for(Milliseconds(50));
        let _ = writeln!(console, "throttle-probe: {label} output {}", actual());
    }
}

fn main() {
    let mut console = Console::writer();

    if TockSyscalls::command(THROTTLE, CMD_ARM, 0, 0)
        .to_result::<(), ErrorCode>()
        .is_err()
    {
        let _ = writeln!(console, "throttle-probe: could not arm");
        return;
    }
    let _ = writeln!(console, "throttle-probe: armed, output {}", actual());

    let _ = writeln!(console, "throttle-probe: PHASE 1 asking for full");
    watch(&mut console, "slew-up", 600, Some(SCALE));

    let _ = writeln!(
        console,
        "throttle-probe: PHASE 2 going silent -- reading only, not asking"
    );
    watch(&mut console, "silent", 900, None);

    let _ = writeln!(console, "throttle-probe: PHASE 3 asking again");
    watch(&mut console, "recover", 600, Some(SCALE));

    let _ = writeln!(
        console,
        "throttle-probe: PHASE 4 terminating with the throttle OPEN at {}",
        actual()
    );
}
