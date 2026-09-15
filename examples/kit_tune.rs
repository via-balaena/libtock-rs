//! Plays four notes on the kit's beeper, on a button press, and times each one.
//!
//! This is the successor to `kit_beep`, which drove GP13 by hand and measured
//! what that costs: a constant ~252 us per edge, a ceiling near 2 kHz, and a
//! per-edge overhead that *compresses* a tune rather than shifting it — 440 Hz
//! three and a half semitones flat, 1320 Hz nearly nine. That example was
//! written to argue for a PWM driver in numbers a reader could check. This one
//! is the caller's half of the answer.
//!
//! # This one makes a noise in a room, so read this part
//!
//! The kit's piezo is loud and unpleasant, which is a stated constraint and not
//! an aesthetic complaint. Three things follow, and they are why this app is
//! shaped the way it is.
//!
//! **It plays nothing at boot.** It waits for a press on GP14 and then plays
//! once. An app that sounds on reset goes off before anyone is ready, cannot be
//! declined, and fires again on every flash — and a test whose result depends on
//! someone catching a moment is worse than one that does not, which applies to
//! the trigger as much as to the reading.
//!
//! **It is about one second long**: four notes, not the fifteen this app played
//! in its first version. Four is all the open question needs, see Measured.
//!
//! **A caller cannot make it quieter.** `PwmBuzzer::buzz` hardcodes the duty
//! cycle at `get_maximum_duty_cycle() / 2` (`buzzer_pwm.rs:96`) and the syscall
//! carries only frequency and duration, so volume is not exposed at all. The
//! levers up here are fewer notes, shorter notes, and pitch — a piezo peaks hard
//! near its resonance, so staying low helps. Making quiet a property rather than
//! a workaround needs duty to become a parameter, which is a kernel change.
//!
//! # What this app can and cannot establish
//!
//! `PwmBuzzer::buzz` starts the PWM and then sets an **alarm** for the note's
//! duration (`capsules/extra/src/buzzer_pwm.rs:93-103`). The completion upcall
//! is fired by that alarm, not by anything the PWM block does. So a PWM that is
//! doing nothing at all produces silence with *perfect* timing, and every line
//! this app prints would still say ok.
//!
//! - **Checkable here:** that the driver exists, that a tone command is
//!   accepted, that exactly one completion upcall arrives per note, that it
//!   carries status 0, and that the note lasts roughly as long as it was asked
//!   to. That is the syscall contract and the kernel alarm path.
//! - **Not checkable here, at all:** whether any sound came out, and whether its
//!   pitch matches the frequency requested. No syscall can observe GP13 while
//!   the PWM owns it.
//!
//! **The ear is the only end-to-end instrument**, and an SWD dump of the PWM
//! block cannot stand in for it: that sees one component, not the path. The
//! outcomes a listener can separate:
//!
//! - **Silence** — no square wave arrives at the transducer. Note the size of
//!   that claim: it covers the block not running, the block running into a pad
//!   that was never muxed to PWM, a pin not wired where the silkscreen says, and
//!   a dead transducer. It is not "the PWM is broken", and reading it that
//!   narrowly cost a round here.
//! - **Four notes that do not change pitch** — a wave arrives but its frequency
//!   does not track the request. A timing-only check is completely blind to it.
//! - **Four rising notes** — the path works, from syscall to air.
//!
//! # Measured
//!
//! Pico 2 W, kernel `e9318732a`, 2026-09-10, run by the kernel session. Fifteen
//! of fifteen notes, every one within 1 ms of the 180 ms asked, zero faults, and
//! `stop Err(OFF)` — which is correct, not a fault: command 3 answers `OFF` when
//! nothing is playing (`buzzer_driver.rs:267-273`).
//!
//! **And the beeper was silent.** Every SWD read of slice 6 was true — enabled,
//! counter moving, divider falling as the scale rose, 50% duty in the correct
//! half word — and `IO_BANK0`'s `GPIO13_CTRL` held `0x1f`, FUNCSEL 31, NULL. The
//! pad had never been switched to PWM and the slice was running into a
//! disconnected pin. The PWM driver does not set the pin function and does not
//! claim to. Fixed in `3a8ab0d37`.
//!
//! An earlier version of this file said both failure modes had been "excluded by
//! measurement rather than by listening". That was wrong, and wrong in the
//! direction this app existed to guard against: a register dump of a correctly
//! running block is byte-identical whether or not its output reaches a pin, so
//! it could not tell the two apart. **A measurement of a component is not a
//! measurement of the path it sits on.**
//!
//! **Then it was heard.** After the mux fix a tune played — so the path is good
//! end to end. It then stuck on at a high pitch and the board had to be
//! unplugged and BOOTSEL-booted to silence it.
//!
//! ## Two things that follow for a caller
//!
//! **A stuck beeper cannot be silenced from userspace.** Command 3 returns `OFF`
//! without calling stop when `active_app` is none (`buzzer_driver.rs:267-273`),
//! and the capsule clears `active_app` in `buzzer_done` when the note's alarm
//! fires. So once the capsule believes the note finished, there is nothing for a
//! caller to call. This app issues command 3 on every exit path regardless,
//! which is right when a note is genuinely outstanding and a no-op otherwise.
//!
//! **A frozen pin level is not a candidate explanation for the stuck tone**, and
//! `kit_beep` is why. It held GP13 steady high for 300 ms, three times, and got
//! click pairs at the edges with silence between (`kit_beep.rs:137-148`) — which
//! is what established the transducer is passive. A passive transducer converts
//! transitions, not levels, so a pin frozen mid-cycle clicks once and goes
//! quiet. Whatever was heard, edges were still being driven.
//!
//! ## The scale found something a beep would not have
//!
//! This survives all of the above, because it is a fact about the divider the
//! driver computed rather than about anything reaching a pin. The four dividers
//! read over SWD were this app's D5, E5, F5 and G5. At 125 MHz with
//! `top+1 = 65536`, the slice's output is `sys_clk / (div * 65536)` where `div`
//! is the register over 16:
//!
//! ```text
//! note  reg   div     generated  asked   error
//! D5    0x33  3.1875    598.4    587    +1.94%   +33.3 cents
//! E5    0x2e  2.8750    663.4    659    +0.67%   +11.6 cents
//! F5    0x2b  2.6875    709.7    698    +1.68%   +28.8 cents
//! G5    0x26  2.3750    803.1    784    +2.44%   +41.7 cents
//! ```
//!
//! Every note sharp, and the register says why: the ideal dividers times sixteen
//! are 51.99, 46.31, 43.72 and 38.93, and the hardware held 51, 46, 43 and 38.
//! Four of four truncated, none rounded. **The driver takes the floor.**
//! Inherited from the RP2040 driver rather than new, and rounding to nearest can
//! overshoot at the top of the range, so it is recorded rather than patched.
//!
//! The audible consequence is not "a bit sharp", because the errors are uneven,
//! and uneven errors bend the *intervals* rather than transposing the scale:
//!
//! ```text
//! D5-E5   should be 200.3 cents, generated 178.6   -21.7  a narrow whole tone
//! E5-F5   should be  99.5 cents, generated 116.8   +17.2  a wide semitone
//! F5-G5   should be 201.2 cents, generated 214.0   +12.9
//! ```
//!
//! A semitone at 117 cents next to a whole tone at 179 flattens the difference
//! between the two steps, which is what makes a scale sound sour rather than
//! merely transposed.
//!
//! **This is the open question, and it is why the app now plays exactly these
//! four notes.** They are the ones whose dividers were actually read, so they
//! carry the whole prediction. A listener should hear D-E too narrow and E-F too
//! wide. Hearing that confirms four register reads by ear. Hearing four evenly
//! spaced steps means the arithmetic above is wrong somewhere. "A little song
//! played" does not settle it either way — it needs someone listening for
//! interval quality rather than being startled by what came after.
//!
//! # ABI
//!
//! Read from `capsules/extra/src/buzzer_driver.rs` at `e9318732a`, and
//! `capsules/core/src/driver.rs:97` for the number.
//!
//! ```text
//! driver   0x90000
//! command  0 exists, 1 play when available, 2 play now, 3 stop
//!          1 and 2 take data1 = frequency_hz, data2 = duration_ms
//! upcall   0 fires once per note, on completion, arg0 = statuscode
//! ```
//!
//! This app uses command 1, not 2. Command 2 replaces whatever is sounding and
//! returns `RESERVE` if another app holds the buzzer (`:254-264`), so a melody
//! built on it has to keep its own note clock in userspace and cut each note off
//! by hand — precisely the timing job `kit_beep` measured and failed. With
//! command 1 the duration lives in the kernel next to the alarm enforcing it,
//! and it plays immediately when nothing else holds the buzzer (`:128-136`), so
//! waiting costs nothing.
//!
//! The queue is **one note deep per process**, not a melody buffer: a second
//! command 1 while your own note sounds is stored as `pending_command`, a third
//! returns `NOMEM` (`:137-153`). Gapless playback is therefore possible, since
//! the kernel starts the pending note from `buzzer_done` before userspace sees
//! the upcall (`:190-205`), but this app plays one at a time so each measured
//! duration belongs to exactly one note.
//!
//! Durations are clamped to `DEFAULT_MAX_BUZZ_TIME_MS`, 5000 ms (`:80`), at the
//! capsule rather than rejected, so asking for longer is silently shortened.
//!
//! # Measurement resolution
//!
//! Note length is measured by polling every `POLL_MS` between `get_milliseconds`
//! readings, so each figure carries about that much slop. Enough to catch a note
//! that never ends or ends immediately; not enough to say anything about a few
//! percent of drift. These are not a calibration.

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use core::time::Duration;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buzzer::Buzzer;
use libtock::console::Console;
use libtock::gpio;
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};
use libtock_buzzer::BuzzerListener;
use libtock_platform::{share, ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

const DRIVER_NUM: u32 = 0x90000;
/// Command 3. `libtock_buzzer` does not expose it.
const COMMAND_STOP: u32 = 3;

/// One of the kit's two buttons. Nothing sounds until this is pressed.
const START_BUTTON: u32 = 14;

const NOTE_MS: u32 = 250;
const POLL_MS: u32 = 5;
/// Generous against `NOTE_MS`: a note that has not reported by here is not late,
/// it is missing.
const NOTE_TIMEOUT_MS: u32 = 2_000;
/// Long enough that nobody is rushed, short enough that a forgotten board is not
/// waiting all day.
const PRESS_TIMEOUT_MS: u32 = 120_000;

/// The four notes whose dividers were read over SWD, ascending. Between them
/// they carry the whole interval prediction: a narrow whole tone, a wide
/// semitone, then a whole tone.
const NOTES: [(u32, &str); 4] = [(587, "D5"), (659, "E5"), (698, "F5"), (784, "G5")];

fn wait_until(timeout_ms: u32, ready: impl Fn() -> bool) -> bool {
    let mut waited = 0;
    while waited < timeout_ms {
        if ready() {
            return true;
        }
        if Alarm::sleep_for(Milliseconds(POLL_MS)).is_err() {
            return ready();
        }
        waited += POLL_MS;
    }
    ready()
}

/// Blocks until the button is pressed, or the timeout. Returns whether it was
/// pressed. With an internal pull-up, Low is pressed.
fn wait_for_press(console: &mut impl Write) -> bool {
    let pin = match Gpio::get_pin(START_BUTTON) {
        Ok(pin) => pin,
        Err(_) => {
            let _ = writeln!(
                console,
                "kit_tune: GP{START_BUTTON} is not available, so there is no way to ask for \
                 the tune. Not playing."
            );
            return false;
        }
    };
    let pin = match pin.make_input::<gpio::PullUp>() {
        Ok(pin) => pin,
        Err(_) => {
            let _ = writeln!(console, "kit_tune: could not configure GP{START_BUTTON}");
            return false;
        }
    };

    let _ = writeln!(
        console,
        "kit_tune: press the GP{START_BUTTON} button to play four notes (about 1 s). \
         It is loud."
    );

    let pressed = wait_until(PRESS_TIMEOUT_MS, || {
        matches!(pin.read(), Ok(gpio::GpioState::Low))
    });

    if !pressed {
        let _ = writeln!(
            console,
            "kit_tune: no press within {} s — nothing played",
            PRESS_TIMEOUT_MS / 1000
        );
    }
    pressed
}

fn main() {
    let mut console = Console::writer();

    if Buzzer::exists().is_err() {
        let _ = writeln!(
            console,
            "kit_tune: no buzzer driver at {DRIVER_NUM:#x} — this kernel has no buzzer capsule"
        );
        return;
    }

    if !wait_for_press(&mut console) {
        return;
    }

    let done: Cell<Option<u32>> = Cell::new(None);
    let listener = BuzzerListener(|status| done.set(Some(status)));

    let mut played = 0;
    let mut faults = 0;

    let outcome = share::scope(|subscribe| {
        Buzzer::register_listener(&listener, subscribe)?;

        for (freq, name) in NOTES {
            done.set(None);
            let started = Alarm::get_milliseconds().unwrap_or(0);

            if let Err(e) = Buzzer::tone(freq, Duration::from_millis(NOTE_MS as u64)) {
                let _ = writeln!(console, "kit_tune: {name} {freq} Hz — tone rejected: {e:?}");
                faults += 1;
                break;
            }

            let arrived = wait_until(NOTE_TIMEOUT_MS, || done.get().is_some());
            let elapsed = Alarm::get_milliseconds().unwrap_or(started) - started;

            match (arrived, done.get()) {
                (true, Some(0)) => {
                    played += 1;
                    let _ = writeln!(
                        console,
                        "kit_tune: {name:<3} {freq:>4} Hz  asked {NOTE_MS} ms  took {elapsed} ms"
                    );
                }
                (true, Some(status)) => {
                    faults += 1;
                    let _ = writeln!(
                        console,
                        "kit_tune: {name:<3} {freq:>4} Hz  completed with status {status}"
                    );
                }
                _ => {
                    faults += 1;
                    let _ = writeln!(
                        console,
                        "kit_tune: {name:<3} {freq:>4} Hz  NO UPCALL within {NOTE_TIMEOUT_MS} ms"
                    );
                    break;
                }
            }
        }

        Ok::<(), ErrorCode>(())
    });

    // Right when a note is genuinely outstanding — the break paths above leave
    // one playing. A no-op returning OFF otherwise, and no help at all against a
    // buzzer stuck on after the capsule thinks it finished: see Measured.
    let stop = TockSyscalls::command(DRIVER_NUM, COMMAND_STOP, 0, 0).to_result::<(), ErrorCode>();

    if let Err(e) = outcome {
        let _ = writeln!(console, "kit_tune: could not run: {e:?} (stop {stop:?})");
        return;
    }

    let _ = writeln!(
        console,
        "kit_tune: {played} of 4 notes completed, {faults} faults, stop {stop:?}"
    );

    if faults == 0 && played == 4 {
        let _ = writeln!(
            console,
            "kit_tune: syscall contract and note timing are good. That is NOT a sound \
             check. Listen for D-E narrow and E-F wide — evenly spaced steps mean the \
             cents arithmetic in this file is wrong, and silence means the path broke \
             again. If a tone outlasts the four notes, unplug: userspace cannot stop it."
        );
    }
}
