//! Drives a 28BYJ-48 stepper through a ULN2003 board.
//!
//! Wiring, for the Pico 2 W in the breadboard kit:
//!
//! ```text
//! ULN2003 IN1 -> GP18      ULN2003 IN3 -> GP20
//! ULN2003 IN2 -> GP19      ULN2003 IN4 -> GP21
//! ULN2003 5-12V / GND -> a supply that can be cut independently of the Pico
//! ```
//!
//! GP18-21 are contiguous, present in the board's GPIO capsule, and unused by
//! the kit — which occupies GP2-7 (TFT SPI), GP11 (tied low), GP13 (beeper),
//! GP14/GP15 (buttons), GP16/GP17 (LEDs) and GP26/GP27 (joystick). Do not move
//! this to GP14 or GP15: driving a pin as an output while its button is held
//! shorts through the pin driver.
//!
//! Userspace pin numbers are the GP numbers. The board builds a sparse array
//! indexed by pin, so `get_pin(18)` is GP18 with no offset. Indices 0, 1, 23,
//! 24 and 25 are absent and answer NODEVICE — GP0/GP1 are the console UART, and
//! GP23/24/25/29 belong to the radio, deliberately withheld so a process cannot
//! power the CYW43 up underneath the kernel or cut it mid-transfer.
//!
//! # Heat
//!
//! Nothing in Tock releases a GPIO when a process exits. `capsules_core::gpio`
//! keeps no per-process state and has no cleanup hook, so a pin left as an
//! output holds its level for the life of the board; stopping the process from
//! the console freezes the pins rather than resetting them. A stepper left with
//! a coil energised draws current and heats with nobody watching, and no
//! in-application fault handler prevents that.
//!
//! So this app de-energises on its normal exit path, and turns a bounded number
//! of steps rather than looping forever. Neither helps if it faults mid-step.
//! The real mitigations are, in order: power the motor from a supply you can cut
//! independently of the Pico; and know that a BOOTSEL replug is the guaranteed
//! de-energise, since the ROM bootloader leaves every pad an input.
//!
//! # Speed
//!
//! An alarm round trip costs roughly 336 us on this hardware, so a nominal 2 ms
//! step is about 17% long. The ceiling on step rate is syscall overhead, not the
//! motor. That is slower rotation, not incorrect stepping.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

/// ULN2003 IN1..IN4. GP numbers, which are also the userspace indices.
const COILS: [u32; 4] = [18, 19, 20, 21];

/// Half-step sequence. Eight phases, each a bit per coil. Half-stepping gives
/// smoother motion and more positions than the four-phase full-step sequence,
/// at half the angle per step.
const HALF_STEP: [u8; 8] = [
    0b1000, 0b1100, 0b0100, 0b0110, 0b0010, 0b0011, 0b0001, 0b1001,
];

/// The 28BYJ-48 is 64 steps per internal revolution behind a 64:1 gearbox, so
/// one output revolution is 4096 half-steps.
const STEPS_PER_REVOLUTION: u32 = 4096;

const STEP_INTERVAL: Milliseconds = Milliseconds(2);

fn main() {
    let mut console = Console::writer();

    // Held for the whole run: `make_output` borrows the pin, so these must
    // outlive the outputs taken from them.
    let (mut p0, mut p1, mut p2, mut p3) = match (
        Gpio::get_pin(COILS[0]),
        Gpio::get_pin(COILS[1]),
        Gpio::get_pin(COILS[2]),
        Gpio::get_pin(COILS[3]),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        _ => {
            let _ = writeln!(console, "stepper: a coil pin is absent on this board");
            return;
        }
    };

    let mut coils = match (
        p0.make_output(),
        p1.make_output(),
        p2.make_output(),
        p3.make_output(),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => [a, b, c, d],
        _ => {
            let _ = writeln!(console, "stepper: could not configure a coil as output");
            return;
        }
    };

    let _ = writeln!(
        console,
        "stepper: {} half-steps at {} ms",
        STEPS_PER_REVOLUTION, 2
    );

    for step in 0..STEPS_PER_REVOLUTION {
        let phase = HALF_STEP[(step % HALF_STEP.len() as u32) as usize];

        for (index, coil) in coils.iter_mut().enumerate() {
            let energised = phase & (1 << index) != 0;
            let _ = if energised { coil.set() } else { coil.clear() };
        }

        if Alarm::sleep_for(STEP_INTERVAL).is_err() {
            let _ = writeln!(console, "stepper: alarm failed, de-energising");
            break;
        }
    }

    // The last thing on every path out of the loop. A coil left on draws
    // current for as long as the board is powered.
    for coil in coils.iter_mut() {
        let _ = coil.clear();
    }

    let _ = writeln!(console, "stepper: done, coils off");
}
