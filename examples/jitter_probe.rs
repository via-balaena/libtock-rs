//! Measures how late a periodic userspace task actually wakes, on real silicon.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=jitter_probe
//! ```
//!
//! Written for the question "is a capsule meaningfully more deterministic than
//! a process?", which nobody here had measured. Deriving a bound from
//! `round_robin.rs`'s 10 ms quantum is not the same as knowing.
//!
//! # ABSOLUTE DEADLINES, AND WHY THAT IS THE WHOLE INSTRUMENT
//!
//! Wake `n` is due at `t0 + n * period`, computed once from a single `t0`.
//! **It is not `now + period` from inside the callback**, and the difference is
//! the difference between measuring jitter and measuring nothing: a relative
//! re-arm moves the schedule forward every time a wake runs late, so lateness
//! is absorbed into drift and each individual sample looks punctual. The
//! histogram would come out clean on a kernel with 10 ms excursions.
//!
//! This uses the alarm driver's **command 6**, which takes a `reference` and a
//! `dt` and fires at `reference + dt`, so the deadline is computed kernel-side.
//! `libtock_alarm` exposes only `SET_RELATIVE` (command 5) through
//! `sleep_for`, which is why this app issues the syscall itself rather than
//! using the wrapper. Re-arming relatively would also add the unmeasured gap
//! between reading the clock and the kernel arming the timer to *every* sample,
//! biasing the whole run late.
//!
//! # The tick space, which is not the raw timer
//!
//! Commands 1, 2 and 6 all speak the same **left-justified u32**: the driver
//! pads the underlying counter so it wraps at exactly `2**32`, and command 1
//! returns a frequency *scaled to match that padding*
//! (`capsules/core/src/alarm.rs`, arms 1, 2 and 6). So the frequency printed
//! below is the right divisor for these ticks, and `wrapping_*` arithmetic is
//! correct across the wrap. The frequency is printed rather than assumed
//! because 1 MHz is a guess until the driver says so.
//!
//! # Reading the output
//!
//! The histogram is of |error|; `min` and `max` are signed, because a wake can
//! be *early* and that is worth knowing. `max at sample N` says whether the
//! worst case is a startup artifact or steady state.
//!
//! **A clean run proves nothing on its own.** Run this against `cpu_hog` in a
//! second process: if that does not show excursions toward the scheduler
//! quantum, suspect the instrument before believing the kernel — the two ways
//! it goes quietly green are relative scheduling (fixed above) and a hog that
//! yields (see `cpu_hog.rs`).

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::{share, DefaultConfig, ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

/// Alarm driver, its absolute-alarm command, and its upcall slot.
const ALARM: u32 = 0x0;
const SET_ABSOLUTE: u32 = 6;
const CALLBACK: u32 = 0;

/// Requested period. Change and rebuild to sweep — 1000 for 1 kHz, 100 for
/// 10 kHz. A plain `const` on purpose: cargo refingerprints on a source edit,
/// where it does *not* refingerprint on an `option_env!` change, so a build
/// flag here would silently keep the old value.
const PERIOD_US: u32 = 1000;

/// Samples before printing and stopping.
const SAMPLES: u32 = 10_000;

/// |error| buckets: 0 µs, then 2^0..2^15 µs, then an overflow bucket.
const BUCKETS: usize = 18;

fn main() {
    let mut console = Console::writer();

    let freq = match Alarm::get_frequency() {
        Ok(f) => f.0,
        Err(e) => {
            let _ = writeln!(console, "jitter_probe: get_frequency: {e:?}\r");
            return;
        }
    };
    if freq == 0 {
        let _ = writeln!(
            console,
            "jitter_probe: driver reports 0 Hz; cannot convert\r"
        );
        return;
    }

    // Ticks per period, and the residue the integer division throws away. A
    // period that is not a whole number of ticks cannot be scheduled exactly,
    // and the leftover would otherwise look like a constant bias in the
    // results rather than a property of the request.
    let period_ticks = ((PERIOD_US as u64 * freq as u64) / 1_000_000) as u32;
    let exact = (period_ticks as u64 * 1_000_000) / freq as u64;
    let _ = writeln!(
        console,
        "jitter_probe: alarm freq {freq} Hz (left-justified scale, read from the driver)\r\n\
         \x20 period requested {PERIOD_US} us -> {period_ticks} ticks -> {exact} us actual\r\n\
         \x20 {SAMPLES} samples, absolute deadlines at t0 + n*period via command 6\r"
    );
    if period_ticks == 0 {
        let _ = writeln!(
            console,
            "jitter_probe: period rounds to 0 ticks at this frequency; nothing to measure\r"
        );
        return;
    }

    let t0 = match Alarm::get_ticks() {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(console, "jitter_probe: get_ticks: {e:?}\r");
            return;
        }
    };

    let mut hist = [0u32; BUCKETS];
    let mut min_err = i32::MAX;
    let mut max_err = i32::MIN;
    let mut min_at = 0u32;
    let mut max_at = 0u32;
    let mut early = 0u32;
    let mut errors = 0u32;

    for n in 1..=SAMPLES {
        let dt = period_ticks.wrapping_mul(n);
        let expected = t0.wrapping_add(dt);

        if let Err(e) = sleep_until(t0, dt) {
            errors += 1;
            if errors <= 3 {
                let _ = writeln!(console, "jitter_probe: sample {n}: {e:?}\r");
            }
            continue;
        }

        let now = match Alarm::get_ticks() {
            Ok(t) => t,
            Err(_) => {
                errors += 1;
                continue;
            }
        };

        // Signed difference across a u32 that wraps at 2**32: the subtraction
        // wraps, and reinterpreting as i32 recovers the sign for any error
        // inside +/- 2**31 ticks.
        let err_ticks = now.wrapping_sub(expected) as i32;
        let err_us = (err_ticks as i64 * 1_000_000) / freq as i64;
        let err_us = err_us as i32;

        if err_us < min_err {
            min_err = err_us;
            min_at = n;
        }
        if err_us > max_err {
            max_err = err_us;
            max_at = n;
        }
        if err_us < 0 {
            early += 1;
        }
        hist[bucket(err_us.unsigned_abs())] += 1;
    }

    let _ = writeln!(
        console,
        "jitter_probe: |error| histogram, us -- bucket label is the lower bound\r"
    );
    for (i, count) in hist.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        if i == BUCKETS - 1 {
            let _ = writeln!(console, "  >=65536 us : {count}\r");
        } else if i == 0 {
            let _ = writeln!(console, "        0 us : {count}\r");
        } else {
            let _ = writeln!(console, "  {:>7} us : {count}\r", 1u32 << (i - 1));
        }
    }

    let _ = writeln!(
        console,
        "jitter_probe: min {min_err} us at sample {min_at}, \
         max {max_err} us at sample {max_at}\r\n\
         \x20 {early} of {SAMPLES} woke early, {errors} syscall errors\r\n\
         \x20 A histogram with nothing above a few hundred us on a CONTENDED run \
         means the\r\n\
         \x20 instrument is suspect, not the scheduler. Check cpu_hog is resident \
         and spinning.\r"
    );
}

/// Blocks until `reference + dt` in the driver's left-justified tick space.
///
/// Uses command 6 so the deadline is absolute and computed kernel-side. The
/// upcall carries `(when, reference)`; both are ignored — the measurement is
/// the clock read by the caller after this returns, not what the kernel
/// reports, because the question is when the *process* ran.
fn sleep_until(reference: u32, dt: u32) -> Result<(), ErrorCode> {
    let called: Cell<Option<(u32, u32)>> = Cell::new(None);
    share::scope(|subscribe| {
        TockSyscalls::subscribe::<_, _, DefaultConfig, ALARM, CALLBACK>(subscribe, &called)?;
        TockSyscalls::command(ALARM, SET_ABSOLUTE, reference, dt).to_result::<u32, ErrorCode>()?;
        loop {
            TockSyscalls::yield_wait();
            if called.get().is_some() {
                return Ok(());
            }
        }
    })
}

/// 0 -> bucket 0; 1 -> 1; 2..3 -> 2; 4..7 -> 3; ... 32768..65535 -> 16;
/// everything larger -> the overflow bucket.
fn bucket(us: u32) -> usize {
    if us == 0 {
        return 0;
    }
    let idx = (32 - us.leading_zeros()) as usize; // floor(log2(us)) + 1
    if idx >= BUCKETS - 1 {
        BUCKETS - 1
    } else {
        idx
    }
}
