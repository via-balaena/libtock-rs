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
//! Never drive GP14 as an output. A button held closed while the pin drives the
//! other way is a short through the pin driver.

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

/// The kit's left button. `kit_buttons.rs` is how the wiring was established.
const STOP_BUTTON: u32 = 14;

/// One output revolution of a 28BYJ-48 in half-steps at 2 ms each — about eight
/// seconds, which is long enough to get a finger to a button.
const STEPS: u32 = 4096;
const INTERVAL: Interval = Interval(2000);

fn main() {
    let mut console = Console::writer();

    if Stepper::exists().is_err() {
        let _ = writeln!(console, "no stepper capsule: this needs the bench kernel");
        return;
    }

    let button = match Gpio::get_pin(STOP_BUTTON) {
        Ok(pin) => pin,
        Err(_) => {
            let _ = writeln!(console, "GP{STOP_BUTTON} is not exposed by this kernel");
            return;
        }
    };
    let button = match button.make_input::<gpio::PullUp>() {
        Ok(input) => input,
        Err(_) => {
            let _ = writeln!(console, "could not make GP{STOP_BUTTON} an input");
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
    let press_edge = match resting {
        GpioState::High => PinInterruptEdge::Falling,
        GpioState::Low => PinInterruptEdge::Rising,
    };

    let _ = writeln!(
        console,
        "GP{STOP_BUTTON} rests {resting:?}; turning {STEPS} steps, press to stop"
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
        // for every multiple of 512 and close enough elsewhere.
        Ok(taken) => {
            let _ = writeln!(
                console,
                "stopped after {taken} of {STEPS} steps, about {} degrees",
                taken * 360 / STEPS
            );
        }
        Err(error) => {
            let _ = writeln!(console, "movement failed: {error:?}");
        }
    }
}
