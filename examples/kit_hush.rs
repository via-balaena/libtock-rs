//! **DO NOT FLASH THIS TO QUIET A BEEPER. It reproduces the stuck tone.**
//!
//! This was written as a rescue tool. On its first and only run it printed a
//! clean enable-disable cycle and then the beeper sounded constantly and loudly
//! until the kernel session killed the slice over SWD. Nobody pressed anything;
//! the other app on the board never got as far as its prompt. See Measured.
//!
//! It is kept, and it still earns its place, but as a **reproducer**: it is the
//! shortest path to the stuck state and the only one that provokes it on demand.
//! Treat flashing it as deliberately making a loud noise in a room, with the
//! plug as the only certain stop.
//!
//! Its original purpose — an off switch that is not the power lead — is not
//! served by this code and is not currently served by anything. The section
//! below on why a stuck beeper is unreachable from userspace is still correct,
//! and is now the whole story rather than a problem this app solves.
//!
//! # Why a stuck beeper is unreachable
//!
//! Command 3 is "stop", and it does nothing in exactly the state you need it in.
//! `buzzer_driver.rs:267-273`:
//!
//! ```text
//! !is_valid_app(me)      -> RESERVE   another process owns the buzzer
//! active_app.is_none()   -> OFF       returned WITHOUT calling stop
//! otherwise              -> stop
//! ```
//!
//! `active_app` is cleared in `buzzer_done` (`:190-205`), which runs when the
//! note's alarm fires. So the moment a note completes, the capsule believes the
//! buzzer is idle — and if the hardware is still making a noise at that point,
//! command 3 answers `OFF` and changes nothing. The state is outside the model,
//! so there is no call to make.
//!
//! # The way in
//!
//! Command 1 does not care whether the hardware is quiet; it cares whether
//! `active_app` is none (`:128-136`). So asking for a note re-acquires the
//! buzzer, sets `active_app`, and calls `PwmBuzzer::buzz`, which calls
//! `pwm_pin.start(..)` and arms an alarm (`buzzer_pwm.rs:93-103`). When that
//! alarm fires, the handler calls `pwm_pin.stop()` (`buzzer_pwm.rs:117-121`).
//!
//! That is a full enable-then-disable cycle on the slice. **If the stuck state
//! is the enable bit left set, this clears it.** If the stuck state is something
//! else, this will not help and the plug is still the answer — which is worth
//! knowing either way, and is why this app reports rather than just acting.
//!
//! Asking for a note means making a noise, so the note is 1 ms: long enough to
//! be an unambiguous duration rather than a special case, short enough to be a
//! click. The frequency is low for the same reason — a piezo is far less
//! unpleasant well below its resonance.
//!
//! # It was supposed not to be able to leave things worse
//!
//! This section used to claim the app could not turn the buzzer on and fail to
//! turn it off: the note is requested, a bounded wait, then command 3 again
//! regardless, so either the alarm stopped it or the explicit stop did. Either
//! way, it said, the slice ends disabled.
//!
//! **That was wrong, and it is the most important thing in this file.** The
//! guarantee rested entirely on `stop` doing what it says — and `stop` is the
//! mechanism under investigation. A safety property that depends on the
//! behaviour being tested is not a safety property; it is the hypothesis wearing
//! a seatbelt. Both stops were issued, both returned what a working stop
//! returns, and the slice kept driving.
//!
//! The sequence below is still the right sequence. What is retracted is the
//! guarantee: it makes the failure *less likely*, not impossible, and nothing a
//! caller can write makes it impossible, because every lever userspace has runs
//! through the same `stop`.
//!
//! # Reading the output
//!
//! - `first stop Ok` — something was playing and this process could stop it.
//!   Nothing exotic happened.
//! - `first stop RESERVE` — another process owns the buzzer. This app does not
//!   interfere; stopping someone else's playback is not a rescue. Terminate that
//!   process instead.
//! - `first stop OFF` then a note and a second stop — the interesting path. The
//!   capsule thought it was idle, so the enable-disable cycle was run. **Whether
//!   the room went quiet is not in this output**; a listener has to say.
//!
//! # Measured
//!
//! Pico 2 W, kernel `3a8ab0d37`, 2026-09-10, run by the kernel session. Both
//! this app and `kit_tune` were resident; this one ran first, at boot, while the
//! room was quiet. Clean boot, and then:
//!
//! ```text
//! kit_hush: first stop Err(OFF)
//! kit_hush: the capsule believes nothing is playing ... running the enable-disable cycle.
//! kit_hush: cycle complete — note finished on its own alarm, slice disabled
//!           by the kernel. second stop OFF, which is correct.
//! kit_hush: the slice has been through enable and disable. Whether the room is
//!           quiet is not something this app can see ...
//! ```
//!
//! Then the beeper sounded constantly and loudly until the slice was killed over
//! SWD. Every line above is the app believing its own syscalls; the last one is
//! the only true thing it said.
//!
//! **So the stuck state is not the enable bit.** That was the leading hypothesis
//! and this run killed it: the cycle completed, the kernel reported the slice
//! disabled, and it kept driving. A queued kernel fix aimed at how `stop` leaves
//! the output level would not have helped, and it is still queued rather than
//! committed.
//!
//! That result is the thing this app was actually for — it was written to be an
//! experiment that doubled as a rescue, and it turned out to be only the
//! experiment. The cost was borne by whoever was in the room.
//!
//! **The register dump that would have named the cause was not taken.** The
//! stuck state was live under a debugger, and the noise was stopped by writing
//! zeros to CSR, EN and `GPIO13_CTRL` without reading them first. Stopping the
//! noise was the right priority. One read before three writes would have cost a
//! fraction of a second, and it is the read that would have said what was still
//! driving edges.
//!
//! ## What is still open
//!
//! `stop` was reached: the completion upcall carries the result of
//! `pwm_pin.stop()`, so the call happened and the slice kept driving anyway.
//!
//! **Jon heard it: "it was high and loud and noisy".** That one answer killed
//! the leading candidate. This app asked for 220 Hz and the earlier incident
//! followed a scale ending on C5 at 523 Hz; neither is high. So the slice was
//! *not* still driving the note last requested, which **disproves** the idea
//! that the PWM mux discarded the Stop and returned `Ok` — that would have left
//! the requested frequency sounding. Both incidents share the high signature, so
//! this is most likely one bug reproduced rather than two.
//!
//! Consistent with, and not established: the reset-default divider. `int=1
//! frac=0` is raw `0x10`, and 125 MHz over a `0xffff` top is ~1907 Hz — high,
//! and near where a piezo resonates, which fits "loud" as well as "high". A
//! post-panic dump did read `CH6 DIV = 0x10`, but that was the *silent* state,
//! not the stuck one, and those are different measurements.
//!
//! Two eliminations, both by reading rather than by hardware:
//!
//! - `start_pwm_pin` has no enable-then-configure window. It computes and
//!   returns `INVAL` on every failure path before any hardware write, then sets
//!   top, divider and compare, and `set_enabled(true)` is its last statement.
//! - `configure_channel` — the only other code that enables a slice, and the
//!   only one that writes the divider from a config rather than a computation —
//!   has exactly one caller, `init()`, and the default config it passes carries
//!   `en: false` (`chips/rp2350/src/pwm.rs:607-622, 315-331`).
//!
//! Which tightens the question rather than answering it: within the chip driver
//! there is no runtime path that enables a slice without first writing the
//! divider. So "enabled at the reset-default divider" is not a state this code
//! produces by any route, and the next question is whether the writes are
//! landing rather than which path ran.
//!
//! No fourth mechanism is offered. Two were proposed today and both died to
//! facts already in hand, the second wearing different clothes than the first.
//!
//! One structural difference worth testing, and it is about this app's own
//! parameter rather than the kernel. `NUDGE_MS` is 1 ms, and one period at
//! `NUDGE_HZ` = 220 Hz is 4.5 ms — so the stop lands **before the counter has
//! wrapped even once**, a quarter of the way into the first cycle. `kit_tune`'s
//! notes are 250 ms and wrap many times. That is the one thing structurally
//! unlike the app that stuck the first time, and it is untested.
//!
//! Note what it does not explain on its own: a stop landing mid-cycle leaves the
//! pin at a level, and `kit_beep` measured that a steady level on this passive
//! transducer is silent (`kit_beep.rs:137-148`). A sustained tone needs
//! continuing edges. So "stopped mid-first-cycle" is a lead about *why the stop
//! failed*, not a mechanism for the noise by itself.

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use core::time::Duration;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buzzer::Buzzer;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_buzzer::BuzzerListener;
use libtock_platform::{share, ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

const DRIVER_NUM: u32 = 0x90000;
const COMMAND_STOP: u32 = 3;

/// Short enough to be a click, and a plain duration rather than a zero that
/// would need its own reasoning about alarm behaviour.
const NUDGE_MS: u32 = 1;
/// Low, because a piezo near resonance is what made this worth building.
const NUDGE_HZ: u32 = 220;

const POLL_MS: u32 = 5;
/// Generous against a 1 ms note. Past this the alarm is not late, it is absent,
/// and the second stop below is what handles that.
const DONE_TIMEOUT_MS: u32 = 500;

fn stop() -> Result<(), ErrorCode> {
    TockSyscalls::command(DRIVER_NUM, COMMAND_STOP, 0, 0).to_result::<(), ErrorCode>()
}

fn main() {
    let mut console = Console::writer();

    if Buzzer::exists().is_err() {
        let _ = writeln!(console, "kit_hush: no buzzer driver at {DRIVER_NUM:#x}");
        return;
    }

    // Ask plainly first. If a note is genuinely outstanding and this process may
    // stop it, that is the whole job and no noise is needed.
    let first = stop();
    let _ = writeln!(console, "kit_hush: first stop {first:?}");

    match first {
        Ok(()) => {
            let _ = writeln!(
                console,
                "kit_hush: a note was outstanding and has been stopped. Nothing further."
            );
            return;
        }
        Err(ErrorCode::Reserve) => {
            let _ = writeln!(
                console,
                "kit_hush: another process owns the buzzer. Not interfering — stopping \
                 someone else's playback is not a rescue. Terminate that process instead."
            );
            return;
        }
        Err(ErrorCode::Off) => {
            let _ = writeln!(
                console,
                "kit_hush: the capsule believes nothing is playing. If the room disagrees, \
                 that is the unreachable state — running the enable-disable cycle."
            );
        }
        Err(e) => {
            let _ = writeln!(
                console,
                "kit_hush: unexpected {e:?}, trying the cycle anyway"
            );
        }
    }

    let done: Cell<Option<u32>> = Cell::new(None);
    let listener = BuzzerListener(|status| done.set(Some(status)));

    let requested = share::scope(|subscribe| {
        Buzzer::register_listener(&listener, subscribe)?;
        Buzzer::tone(NUDGE_HZ, Duration::from_millis(NUDGE_MS as u64))?;

        let mut waited = 0;
        while waited < DONE_TIMEOUT_MS && done.get().is_none() {
            if Alarm::sleep_for(Milliseconds(POLL_MS)).is_err() {
                break;
            }
            waited += POLL_MS;
        }
        Ok::<(), ErrorCode>(())
    });

    if let Err(e) = requested {
        // The note was refused, so nothing was started and nothing is owed. Say
        // so rather than implying the cycle ran.
        let _ = writeln!(
            console,
            "kit_hush: could not request the nudge note: {e:?} — no cycle was run"
        );
        let _ = writeln!(console, "kit_hush: final stop {:?}", stop());
        return;
    }

    // Unconditional, and the point of the whole design: if the alarm fired this
    // is a no-op, and if it did not this is what stops the pin.
    let second = stop();

    match (done.get(), second) {
        (Some(_), Err(ErrorCode::Off)) => {
            let _ = writeln!(
                console,
                "kit_hush: cycle complete — note finished on its own alarm, slice disabled \
                 by the kernel. second stop OFF, which is correct."
            );
        }
        (None, Ok(())) => {
            let _ = writeln!(
                console,
                "kit_hush: the note's alarm never fired within {DONE_TIMEOUT_MS} ms and this \
                 process still owned the buzzer, so the explicit stop did it. Worth \
                 reporting — that is a second defect, not a rescue."
            );
        }
        (done, second) => {
            let _ = writeln!(
                console,
                "kit_hush: cycle ran, upcall {done:?}, second stop {second:?}"
            );
        }
    }

    let _ = writeln!(
        console,
        "kit_hush: the slice has been through enable and disable. Whether the room is \
         quiet is not something this app can see — if it is still sounding, the stuck \
         state is not the enable bit and the plug is still the answer."
    );
}
