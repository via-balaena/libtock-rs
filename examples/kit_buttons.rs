//! Reads the breadboard kit's two buttons on GP14 and GP15 via GPIO interrupts.
//!
//! Needs no jumpers: the kit already wires both, and both are exposed by the
//! board's GPIO driver.
//!
//! Deliberately assumes nothing about the wiring. The kit is undocumented, so
//! this enables an internal pull-up, prints the resting level, and takes
//! interrupts on *both* edges. The output tells you the polarity rather than
//! requiring you to know it first — if the buttons pull to ground, a press is
//! High -> Low; if the kit has its own pull-downs, it is the other way round.
//!
//! Never drive GP14 or GP15 as outputs. A button held closed while the pin
//! drives the other way is a short through the pin driver.
//!
//! This is also the first thing here driven by an upcall nobody asked for.
//! Every other driver in this crate starts an operation and waits for its
//! completion; a button edge arrives because a person pressed something.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::gpio;
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::{share, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

const LEFT: u32 = 14;
const RIGHT: u32 = 15;

/// One of the kit's two discrete LEDs. GP17 is the SPI capsule's chip select on
/// the bench kernel, so GP16 is the one that is still a plain pin.
const HEARTBEAT_LED: u32 = 16;

fn main() {
    let mut console = Console::writer();

    let (left, right) = match (Gpio::get_pin(LEFT), Gpio::get_pin(RIGHT)) {
        (Ok(left), Ok(right)) => (left, right),
        _ => {
            let _ = writeln!(console, "kit_buttons: GP14/GP15 are not available");
            return;
        }
    };

    let (left, right) = match (
        left.make_input::<gpio::PullUp>(),
        right.make_input::<gpio::PullUp>(),
    ) {
        (Ok(left), Ok(right)) => (left, right),
        _ => {
            let _ = writeln!(console, "kit_buttons: could not configure inputs");
            return;
        }
    };

    // Resting levels first: with an internal pull-up, High means the button is
    // open and nothing external is holding the line.
    let _ = writeln!(
        console,
        "kit_buttons: resting GP{}={:?} GP{}={:?}",
        LEFT,
        left.read(),
        RIGHT,
        right.read()
    );

    if left
        .enable_interrupts(gpio::PinInterruptEdge::Either)
        .is_err()
        || right
            .enable_interrupts(gpio::PinInterruptEdge::Either)
            .is_err()
    {
        let _ = writeln!(console, "kit_buttons: could not enable interrupts");
        return;
    }

    let _ = writeln!(console, "kit_buttons: press a button");

    let listener = gpio::GpioInterruptListener(|index, state| {
        let _ = writeln!(Console::writer(), "GP{index} -> {state:?}");
    });

    // A heartbeat on the kit's LED, because there is otherwise no way to tell
    // this app from a dead board. Nothing here moves, lights or sounds, so
    // somebody asked to press a button has no cue that the moment has arrived
    // -- and a console they cannot see live is not a cue. That cost two runs.
    // The `Pin` has to outlive the `OutputPin` that borrows it, so it is bound
    // here rather than produced inside a closure.
    let mut led_pin = Gpio::get_pin(HEARTBEAT_LED);
    let mut heartbeat = match led_pin {
        Ok(ref mut pin) => pin.make_output().ok(),
        Err(_) => None,
    };
    if heartbeat.is_none() {
        let _ = writeln!(
            console,
            "kit_buttons: no LED on GP{HEARTBEAT_LED}, watch the console instead"
        );
    }

    share::scope(|subscribe| {
        if Gpio::register_listener(&listener, subscribe).is_err() {
            let _ = writeln!(Console::writer(), "kit_buttons: could not subscribe");
            return;
        }
        loop {
            if let Some(led) = heartbeat.as_mut() {
                let _ = led.toggle();
            }
            // Sleeping rather than `yield_wait` keeps the blink going, and an
            // alarm yields the same way, so button upcalls still arrive.
            if Alarm::sleep_for(Milliseconds(250)).is_err() {
                TockSyscalls::yield_wait();
            }
        }
    });
}
