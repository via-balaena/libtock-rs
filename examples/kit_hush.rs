//! Tries to shut the kit's beeper up without reaching for the power lead.
//!
//! A tune played on this board once and then stuck on at a high pitch, and the
//! only way to stop it was unplugging and a BOOTSEL boot. This app exists so
//! that is not the only option next time. It is a rescue tool and a probe, and
//! it makes at most one click.
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
//! # It cannot leave things worse
//!
//! The risk in a rescue tool is that it turns the buzzer *on* and fails to turn
//! it off, and the sequence is built so that cannot happen. After the note is
//! requested, this app waits a bounded time for the completion upcall and then
//! issues command 3 again regardless. If the alarm fired, the second command 3
//! returns `OFF` and everything is already stopped. If the alarm never fired,
//! `active_app` is still this process, so command 3 takes the `stop` branch and
//! stops it. Either way the slice ends disabled, and which of the two happened
//! is visible in the output.
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
//! Not yet run on hardware.

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
