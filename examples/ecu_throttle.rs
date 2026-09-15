//! A throttle application for the drift trike, on bench stand-ins.
//!
//! The half of the ECU that is NOT safety critical, which is the point. It
//! reads a pedal and a brake and asks `capsules_extra::throttle` for a
//! position; every limit that matters -- slew rate, a timeout on silence, and
//! closing on a dead owner -- is enforced in the capsule, where an application
//! cannot walk around it. The worst this app can do by being wrong is fail to
//! ask for zero, and three other routes to zero still run.
//!
//! # The stand-ins
//!
//! Nothing here needs parts that are not already on the breadboard kit:
//!
//! * **Pedal** -- the joystick's vertical axis on ADC channel 0. Centred
//!   rather than resting at zero like a real hall pedal, so the upper half of
//!   its travel is the pedal and the lower half is ignored.
//! * **Brake** -- button 0, GP14.
//! * **Output** -- GP19, the throttle driver's pin.
//!
//! # The band check
//!
//! A hall throttle sits inside a band and never reaches either rail: below it
//! means a broken wire, above it means a short to the supply. Neither is "no
//! throttle", both are faults, and treating a broken wire as idle is how a
//! pedal that has fallen apart reads as a pedal at rest.
//!
//! The joystick reaches both rails at its hard stops, which is what makes the
//! fault reachable on the bench: push it to an extreme and the app disarms.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::adc::Adc;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buttons::Buttons;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::{ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x400}

/// `capsules_core::driver::NUM::Throttle`.
const THROTTLE: u32 = 0x00012;
const CMD_ARM: u32 = 1;
const CMD_SET: u32 = 2;
const CMD_DISARM: u32 = 3;
const CMD_ACTUAL: u32 = 4;

/// Full scale, matching the capsule's own.
const SCALE: u32 = 10_000;

/// The pedal's axis.
const PEDAL_CHANNEL: u32 = 0;
/// The brake.
const BRAKE_BUTTON: u32 = 0;

/// Outside this the reading is a fault, not a position.
const BAND_LOW: u16 = 1_500;
const BAND_HIGH: u16 = 64_000;

/// Where the stick rests, and where it is asking for everything.
const PEDAL_REST: u16 = 33_000;
const PEDAL_FULL: u16 = 60_000;

/// How often the pedal is read.
const PERIOD_MS: u32 = 50;

fn request(target: u32) -> Result<(), ErrorCode> {
    TockSyscalls::command(THROTTLE, CMD_SET, target, 0).to_result::<(), ErrorCode>()
}

fn main() {
    let mut console = Console::writer();

    if TockSyscalls::command(THROTTLE, 0, 0, 0)
        .to_result::<(), ErrorCode>()
        .is_err()
    {
        let _ = writeln!(console, "ecu: no throttle driver on this board");
        return;
    }

    // Arming closes the throttle and starts the capsule's tick. From here on
    // silence is treated as a request for zero, so this app must keep talking.
    if let Err(e) = TockSyscalls::command(THROTTLE, CMD_ARM, 0, 0).to_result::<(), ErrorCode>() {
        let _ = writeln!(console, "ecu: could not arm the throttle: {e:?}");
        return;
    }
    let _ = writeln!(
        console,
        "ecu: armed. pedal = joystick up, brake = button 0, output on GP19"
    );

    // A pedal that is not at rest when the app starts is not trusted: an
    // application that comes up with the throttle already open is how a
    // vehicle moves before anyone has asked it to.
    let mut ever_at_rest = false;
    // Latched by the brake, cleared only by the pedal returning to rest, so
    // releasing the brake does not resume the throttle where it left off.
    let mut locked_out = false;
    let mut ticks: u32 = 0;

    loop {
        let raw = match Adc::read_single_sample_sync(PEDAL_CHANNEL) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(console, "ecu: pedal read failed {e:?} -- closing");
                let _ = request(0);
                let _ = Alarm::sleep_for(Milliseconds(PERIOD_MS));
                continue;
            }
        };

        // A reading outside the band is a wiring fault. Disarm rather than
        // treat it as a position: the capsule will close the output, and this
        // app stops rather than guessing.
        if raw < BAND_LOW || raw > BAND_HIGH {
            let _ = writeln!(
                console,
                "ecu: pedal {raw} is outside {BAND_LOW}..{BAND_HIGH} -- FAULT, disarming"
            );
            let _ = TockSyscalls::command(THROTTLE, CMD_DISARM, 0, 0).to_result::<(), ErrorCode>();
            return;
        }

        let at_rest = raw <= PEDAL_REST;
        if at_rest {
            ever_at_rest = true;
            locked_out = false;
        }

        let braking = Buttons::is_pressed(BRAKE_BUTTON);
        if braking {
            locked_out = true;
        }

        let target = if braking || locked_out || !ever_at_rest || at_rest {
            0
        } else {
            let span = (PEDAL_FULL - PEDAL_REST) as u32;
            let over = (raw.min(PEDAL_FULL) - PEDAL_REST) as u32;
            over * SCALE / span
        };

        let _ = request(target);

        ticks += 1;
        if ticks % 10 == 0 {
            let actual = TockSyscalls::command(THROTTLE, CMD_ACTUAL, 0, 0)
                .to_result::<u32, ErrorCode>()
                .unwrap_or(0);
            let _ = writeln!(
                console,
                "ecu: pedal {raw:5} target {target:5} output {actual:5}{}{}",
                if braking { " BRAKE" } else { "" },
                if !ever_at_rest { " NOT-ARMED-AT-REST" } else { "" }
            );
        }

        let _ = Alarm::sleep_for(Milliseconds(PERIOD_MS));
    }
}
