//! Spins the motor until you press a button, and says where it stopped.
//!
//! Wants the bench kernel: the stepper capsule on GP18-21 through a ULN2003,
//! and the kit's left button on GP14. Run with
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=stepper_until_press FEATURES=async
//! ```
//!
//! Two futures over two drivers race, and the interesting part is what happens
//! to the loser. `select` drops it, which for the motor would be exactly wrong:
//! dropping a `Step` stops the coils correctly and throws away the step count,
//! and for an open-loop motor that count is the only record of where the shaft
//! is. So the movement is *lent* to `select` rather than given to it —
//! `Pin<&mut F>` is itself a `Future`, so what gets dropped is the borrow. The
//! motor is stopped explicitly, and the same future is then awaited a second
//! time to collect the count.
//!
//! The stop also returns that count directly, so on this driver the lending is
//! belt as well as braces. It is kept because it is the general answer — it
//! holds for any operation whose result arrives in an upcall, while the
//! synchronous return only rescues the ones that also have a stop command that
//! reports. Having both here is useful in its own right: two independent routes
//! to the position, and the example says so if they disagree.
//!
//! The button half is a `gpio::Edge`, which is the first future here over an
//! event nobody requested. It is worth knowing that it samples rather than
//! streams: only presses that land while the future is being awaited are seen,
//! and there is no gap here only because the await covers the whole movement.
//!
//! Never drive GP14 or GP15 as an output. A button held closed while the pin
//! drives the other way is a short through the pin driver.
//!
//! # What it did on hardware, 2026-09-05
//!
//! ```text
//! GP14 rests High; turning 12288 steps, press either button
//! GP15 -> Low, stopping
//! stopped after 2118 of 12288 steps, about 186 degrees
//! ```
//!
//! Four things at once, three of them previously only argued for:
//!
//! * `select` raced two drivers on real silicon and tore the loser down.
//! * **The count survived.** Lending the movement rather than giving it is what
//!   saved it: a given future is dropped by `select`, and `Operation::cancel`
//!   returns nothing, so the motor would have stopped correctly at an angle
//!   nobody could name.
//! * **The two counts agreed.** No disagreement line printed, so `Stepper::stop`'s
//!   synchronous return matched the completion upcall's. Two independent routes
//!   to the same position.
//! * **The press was on GP15 and the future was awaiting GP14.** It resolved
//!   anyway and reported GP15. That is `gpio::Edge`'s documented inability to
//!   decline an upcall, confirmed on hardware -- it had only ever been shown
//!   against a fake written by the same hand that wrote the claim.

#![no_main]
#![no_std]

use core::fmt::Write;
use core::pin::pin;

use libtock::console::Console;
use libtock::futures::{block_on, select, Either};
use libtock::gpio::{self, Gpio, GpioState, PinInterruptEdge};
use libtock::runtime::{set_main, stack_size, TockSyscalls};
use libtock::stepper::{Interval, Stepper};

set_main! {main}
stack_size! {0x800}

/// The kit's two buttons. **GP15 is the left one and GP14 the right**, measured
/// on 2026-09-05 by pressing left twice then right twice and reading the order
/// the edges arrived in. This file previously called GP14 "the left button" on
/// no evidence at all, and had it backwards.
///
/// Either stops the motor. Only `STOP_BUTTON` gets an `Edge` future; the other
/// has its interrupt enabled directly, which is the point — see below.
const STOP_BUTTON: u32 = 14;
const OTHER_BUTTON: u32 = 15;

/// Three output revolutions of a 28BYJ-48 in half-steps at 2 ms each — about
/// twenty-five seconds. One revolution is eight, which is long enough only if
/// you already know when it starts; the extra two are there so nothing has to
/// be caught.
const STEPS_PER_REVOLUTION: u32 = 4096;
const STEPS: u32 = STEPS_PER_REVOLUTION * 3;
const INTERVAL: Interval = Interval(2000);

/// With the internal pull-up on, a resting High means the button pulls the line
/// to ground when it closes, so a press is the falling edge. Measured rather
/// than assumed: the kit is undocumented.
fn press_edge_for(resting: GpioState) -> PinInterruptEdge {
    match resting {
        GpioState::High => PinInterruptEdge::Falling,
        GpioState::Low => PinInterruptEdge::Rising,
    }
}

fn main() {
    let mut console = Console::writer();

    if Stepper::exists().is_err() {
        let _ = writeln!(console, "no stepper capsule: this needs the bench kernel");
        return;
    }

    let (button, other) = match (Gpio::get_pin(STOP_BUTTON), Gpio::get_pin(OTHER_BUTTON)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            let _ = writeln!(
                console,
                "GP{STOP_BUTTON}/GP{OTHER_BUTTON} are not both exposed"
            );
            return;
        }
    };
    let (button, other) = match (
        button.make_input::<gpio::PullUp>(),
        other.make_input::<gpio::PullUp>(),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            let _ = writeln!(console, "could not make the buttons inputs");
            return;
        }
    };

    // The kit is undocumented, so the polarity is measured rather than assumed:
    // with the internal pull-up on, a resting High means the button pulls the
    // line to ground when it closes, and a press is the falling edge.
    let resting = match button.read() {
        Ok(state) => state,
        Err(_) => {
            let _ = writeln!(console, "could not read GP{STOP_BUTTON}");
            return;
        }
    };
    let press_edge = press_edge_for(resting);

    // The `Edge` future owns GP14's interrupt enable. GP15's is turned on here
    // instead, which is the point: driver 4 keeps one upcall slot for the whole
    // process and its capsule broadcasts every edge into it, so a future
    // awaiting GP14 resolves on a GP15 edge and reports GP15's number. That
    // behaviour is documented on `gpio::Edge` and tested against a fake; this
    // is the first time it runs on silicon.
    if other.enable_interrupts(press_edge_for(resting)).is_err() {
        let _ = writeln!(console, "could not enable GP{OTHER_BUTTON}'s interrupt");
        return;
    }

    let _ = writeln!(
        console,
        "GP{STOP_BUTTON} rests {resting:?}; turning {STEPS} steps, press either button"
    );

    let outcome = block_on::<TockSyscalls, _>(async {
        let mut step = pin!(Stepper::step_forward_async(STEPS, INTERVAL));
        let press = pin!(button.next_edge(press_edge));

        match select(step.as_mut(), press).await {
            // Nobody pressed: the movement ran to its end on its own.
            Either::Left(finished) => finished,

            // Somebody pressed. Stop the motor, then await the *same* future
            // again -- it is still subscribed, so the completion upcall the
            // stop provokes lands in it and resolves it with the partial count.
            // Dropping it here instead would stop the motor just as correctly
            // and forget where it stopped.
            Either::Right(edge) => {
                if let Ok((pin, state)) = edge {
                    let _ = writeln!(Console::writer(), "GP{pin} -> {state:?}, stopping");
                }

                let reported = Stepper::stop()?;
                let awaited = step.await?;

                if reported != awaited {
                    let _ = writeln!(
                        Console::writer(),
                        "the two counts disagree: stop said {reported}, the run said {awaited}"
                    );
                }
                Ok(awaited)
            }
        }
    });

    match outcome {
        // 4096 half-steps to the revolution, so the angle is exact in integers
        // for every multiple of 512 and close enough elsewhere. Divided by the
        // steps in a revolution and not by the steps requested -- dividing by
        // STEPS made a three-revolution movement report 360 degrees, which is
        // right only when the request happens to be exactly one turn.
        Ok(taken) => {
            let _ = writeln!(
                console,
                "stopped after {taken} of {STEPS} steps, about {} degrees",
                taken * 360 / STEPS_PER_REVOLUTION
            );
        }
        Err(error) => {
            let _ = writeln!(console, "movement failed: {error:?}");
        }
    }
}
