//! Makes the kit's beeper make a noise, and finds out which kind it is.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_beep
//! ```
//!
//! **This no longer runs on the Pico 2 W, and that is the point of it.** The
//! measurements below argued for a PWM-backed buzzer; the kernel now has one,
//! GP13 belongs to it, and the pin was removed from the board's GPIO array in
//! `4f384c679` so that nothing can fight the PWM for it. So `get_pin(13)` now
//! answers NODEVICE here and this app says so and stops — which is the correct
//! outcome, not a regression. `examples/kit_tune.rs` is the successor, and the
//! note at the top of `main` about `Buzzer::tone` being the right way to do
//! this came true.
//!
//! It is kept because the numbers are the argument that produced the driver,
//! and because it still runs on any board that exposes GP13 as a plain pin.
//!
//! There are two kinds of beeper in these kits and they need opposite things.
//! An *active* one contains its own oscillator: hold the pin high and it
//! sounds, at whatever pitch it was built for. A *passive* one is a bare
//! transducer: hold the pin high and it clicks once and goes quiet, because it
//! needs a square wave at the pitch you want. Driving the wrong one looks
//! exactly like a broken wire.
//!
//! So this does both in turn and says what to listen for, rather than picking
//! one and reporting failure if it guessed wrong.
//!
//! GP13 comes from the kit's silkscreen and has never been confirmed by
//! measurement. If neither phase makes a sound, suspect the pin before the
//! code: `examples/kit_outscan.rs` maps what is actually connected.
//!
//! The square wave is bit-banged from userspace, one syscall per edge, which is
//! the wrong way to make a tone and is worth doing once to see how wrong. Each
//! phase reports the frequency it actually achieved next to the one it asked
//! for. The gap is the cost of a syscall and an alarm per half-cycle, and it is
//! the argument for a PWM driver rather than a bug in this example -- PWM is
//! not yet in the RP2350 crate, which is why this exists at all.
//!
//! # What it did on hardware, 2026-09-05
//!
//! The kit's beeper is **passive**: the three square waves rose in pitch, which
//! an active buzzer cannot do, since its pitch is its own. The three "beeps" of
//! the first phase were the click pairs a bare transducer makes at each edge.
//!
//! ```text
//! asked  440 Hz, achieved 360 Hz over 610783 ticks
//! asked  880 Hz, achieved 609 Hz over 721488 ticks
//! asked 1320 Hz, achieved 793 Hz over 831346 ticks
//! ```
//!
//! Reproducible across runs to a few hundred ticks in six hundred thousand.
//! The alarm runs at 1 MHz, and backing the overhead out of the tick counts
//! gives **252 microseconds per edge at all three frequencies** -- 111,202
//! ticks over 440 edges, 221,348 over 880, 332,372 over 1320. A constant
//! per-iteration cost: one GPIO command plus a whole `sleep_for`, which is a
//! share scope, a subscribe, a command, a yield, an upcall and an unsubscribe.
//!
//! Two things follow, and the second is the interesting one.
//!
//! The ceiling is about **2 kHz** even with a zero-length requested delay,
//! because 252 us is the floor on a half-period.
//!
//! And a constant per-edge cost does not shift a tune, it **compresses** it:
//! the higher the note, the larger the fraction of its period the overhead
//! eats. 440 Hz plays 3.5 semitones flat, 1320 Hz nearly nine. No melody
//! survives that. "Userspace timing is imprecise" is a weaker way to say it
//! than "the instrument plays flat, and worse the higher you go".

#![no_main]
#![no_std]

use core::fmt::Write;

use libtock::alarm::{Alarm, Hz, Milliseconds, Ticks};
use libtock::buzzer::Buzzer;
use libtock::console::Console;
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x800}

/// The kit's beeper, per the silkscreen.
const BEEPER: u32 = 13;

/// A, one octave up, and an E above that. Spread far enough apart that a
/// passive transducer with an uneven response still shows something.
const TONES: [u32; 3] = [440, 880, 1320];
const TONE_MS: u32 = 500;

fn main() {
    let mut console = Console::writer();

    // Worth saying either way. If a board ever gains a PWM-backed buzzer
    // capsule, `Buzzer::tone` is the right way to do this and all the
    // bit-banging below becomes a curiosity.
    let _ = writeln!(
        console,
        "kit_beep: buzzer capsule {}",
        match Buzzer::exists() {
            Ok(()) => "present -- prefer it over this example",
            Err(_) => "absent, bit-banging GP13 instead",
        }
    );

    let frequency = match Alarm::get_frequency() {
        Ok(Hz(frequency)) => frequency,
        Err(_) => {
            let _ = writeln!(console, "kit_beep: no alarm driver");
            return;
        }
    };

    let mut pin = match Gpio::get_pin(BEEPER) {
        Ok(pin) => pin,
        Err(_) => {
            let _ = writeln!(
                console,
                "kit_beep: GP{BEEPER} is not exposed by this kernel"
            );
            return;
        }
    };
    let mut pin = match pin.make_output() {
        Ok(output) => output,
        Err(_) => {
            let _ = writeln!(console, "kit_beep: could not drive GP{BEEPER}");
            return;
        }
    };

    // Phase one: hold it high. An active buzzer sounds for each of the three
    // seconds; a passive one ticks twice per second and is otherwise silent.
    let _ = writeln!(
        console,
        "kit_beep: holding GP{BEEPER} high 3 times -- an ACTIVE buzzer sounds now"
    );
    for _ in 0..3 {
        let _ = pin.set();
        if Alarm::sleep_for(Milliseconds(300)).is_err() {
            return;
        }
        let _ = pin.clear();
        if Alarm::sleep_for(Milliseconds(300)).is_err() {
            return;
        }
    }

    // Phase two: square waves. A passive buzzer sounds three rising notes; an
    // active one drones at its own pitch regardless, which is itself the answer.
    let _ = writeln!(
        console,
        "kit_beep: three square waves -- a PASSIVE buzzer sounds now"
    );

    for tone in TONES {
        let half_period = frequency / (2 * tone);
        if half_period == 0 {
            let _ = writeln!(
                console,
                "  {tone} Hz needs a finer tick than this alarm's {frequency} Hz, skipped"
            );
            continue;
        }

        let cycles = tone * TONE_MS / 1000;
        let started = Alarm::get_ticks().unwrap_or(0);

        for _ in 0..cycles {
            let _ = pin.set();
            if Alarm::sleep_for(Ticks(half_period)).is_err() {
                return;
            }
            let _ = pin.clear();
            if Alarm::sleep_for(Ticks(half_period)).is_err() {
                return;
            }
        }

        // What it actually managed. Ticks are a free-running counter, so a wrap
        // during a half-second tone is handled rather than printed as nonsense.
        let elapsed = Alarm::get_ticks().unwrap_or(started).wrapping_sub(started);
        let achieved = if elapsed > 0 {
            (cycles as u64 * frequency as u64 / elapsed as u64) as u32
        } else {
            0
        };
        let _ = writeln!(
            console,
            "  asked {tone} Hz, achieved {achieved} Hz over {elapsed} ticks"
        );

        if Alarm::sleep_for(Milliseconds(200)).is_err() {
            return;
        }
    }

    // Dropping the OutputPin disables GP13, but say so rather than leaving a
    // reader to infer that the beeper is off.
    let _ = pin.clear();
    let _ = writeln!(console, "kit_beep: done, GP{BEEPER} released");
}
