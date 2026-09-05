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

    share::scope(|subscribe| {
        if Gpio::register_listener(&listener, subscribe).is_err() {
            let _ = writeln!(Console::writer(), "kit_buttons: could not subscribe");
            return;
        }
        loop {
            TockSyscalls::yield_wait();
        }
    });
}
