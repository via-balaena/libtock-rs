//! Can a userspace `write` ever meet a driver that is busy? App B, the probe.
//!
//! ```text
//! make raspberry_pi_pico_2_w_slot2 EXAMPLE=screen_busy_probe
//! ```
//!
//! Run against `screen_busy_load` in slot 1, which holds the driver with
//! back-to-back full-screen fills.
//!
//! # The question
//!
//! `screen_first_draw` established that on the boot path the command meeting
//! the readiness gap is `set_write_frame`, which carries no buffer; `write`
//! does not run until that returns, by which time the driver is Idle. So the
//! screen driver's `'static` write buffer is never offered to a driver that
//! can refuse it — **through that path**. Whether any path reaches it is open,
//! and two processes contending is the obvious candidate, because the capsule
//! serialises on `current_process` and what two processes do to that queue has
//! not been traced.
//!
//! This app is the instrument, not the answer. It reports raw command return
//! registers and raw upcall arguments, exactly as `screen_first_draw` does,
//! and a clean run means **not observed under this contention** — never
//! "impossible". The failure it is watching for has never been seen here, so
//! there is no known-positive control; that is a real limit of the test and
//! not a thing the output can talk you out of.
//!
//! # Why it does not print per attempt
//!
//! A console line blocks for the UART — about 8 ms at 115200 for a line this
//! long. An app that printed every attempt would spend nearly all of its time
//! transmitting rather than calling the screen, and the contention the test
//! exists to create would be gone. **The instrument would prevent the thing it
//! measures.** So it counts, keeps the first few anomalies verbatim, and
//! prints once at the end.
//!
//! # The latency spread is the corroboration
//!
//! If the capsule is serialising correctly, this app's small writes queue
//! behind app A's full-screen fills, which are about 40 ms each. So the max
//! should land near a fill and the min near an uncontended write. A max that
//! looks uncontended means app A was not actually running, which would make a
//! clean result meaningless — check that before reading anything into it.

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::allow_ro::AllowRo;
use libtock_platform::share;
use libtock_platform::subscribe::Subscribe;
use libtock_platform::{CommandReturn, DefaultConfig, ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x2000}

const SCREEN: u32 = 0x9_0001;
const SCREEN_EXISTS: u32 = 0;
const SET_WRITE_FRAME: u32 = 100;
const WRITE: u32 = 200;
const SCREEN_CB: u32 = 0;
const WRITE_BUF: u32 = 0;

const ALARM: u32 = 0x0;
const ALARM_STOP: u32 = 3;
const ALARM_SET_RELATIVE: u32 = 5;
const ALARM_CB: u32 = 0;

/// A small band, in the top-left corner, so app A's full-screen fills paint
/// over it and the two are telling apart on the panel.
const BAND_W: u32 = 64;
const BAND_H: u32 = 8;
const BAND_BYTES: usize = (BAND_W * BAND_H) as usize * 2;
const MAGENTA: u16 = 0xF81F;

/// Long enough that a stalled call is unambiguous, and longer than the ~1,240
/// ms readiness gap so the first call is not mistaken for a hang.
const UPCALL_TIMEOUT_MS: u32 = 2_000;

/// How long to hammer, after the panel is ready.
const RUN_MS: u32 = 10_000;

/// Anomalies kept verbatim. Enough to see whether they are all alike.
const KEEP: usize = 4;

enum Outcome {
    Refused,
    Upcall(u32, u32, u32),
    Timeout,
    Setup(ErrorCode),
}

struct Step {
    cmd: (u32, u32, u32, u32),
    outcome: Outcome,
}

impl Step {
    fn is_clean(&self) -> bool {
        matches!(self.outcome, Outcome::Upcall(0, _, _))
    }
}

impl core::fmt::Display for Step {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (r0, r1, r2, r3) = self.cmd;
        write!(f, "cmd=({r0},{r1},{r2},{r3}) ")?;
        match self.outcome {
            Outcome::Refused => write!(f, "refused"),
            Outcome::Upcall(a, b, c) => write!(f, "up=({a},{b},{c})"),
            Outcome::Timeout => write!(f, "TIMEOUT"),
            Outcome::Setup(e) => write!(f, "setup-failed({e:?})"),
        }
    }
}

fn main() {
    let mut console = Console::writer();

    let hz = match Alarm::get_frequency() {
        Ok(f) if f.0 > 0 => f.0,
        _ => {
            let _ = writeln!(console, "busy_probe: no usable alarm frequency\r");
            return;
        }
    };
    if !scheduled(&TockSyscalls::command(SCREEN, SCREEN_EXISTS, 0, 0)) {
        let _ = writeln!(
            console,
            "busy_probe: no screen driver at 0x90001 -- build the board with \
             kit_display\r"
        );
        return;
    }

    let mut band = [0u8; BAND_BYTES];
    for pair in band.chunks_exact_mut(2) {
        pair.copy_from_slice(&MAGENTA.to_be_bytes());
    }
    let timeout_ticks = ((UPCALL_TIMEOUT_MS as u64 * hz as u64) / 1_000) as u32;

    // First call absorbs the readiness gap: it is queued and served at
    // `screen_is_ready`, so everything after it contends with app A rather
    // than with the init sequence. Timing starts after it for that reason.
    let frame = frame_step(0, 0, BAND_W, BAND_H, timeout_ticks);
    let ready_at = Alarm::get_ticks().unwrap_or(0);
    let _ = writeln!(
        console,
        "busy_probe: ready at t={} ms, first frame {frame}\r\n\
         \x20 hammering {BAND_W}x{BAND_H} writes for {RUN_MS} ms, reporting only \
         anomalies\r",
        ms(ready_at, hz)
    );

    let mut attempts = 0u32;
    let mut clean = 0u32;
    let mut refused = 0u32;
    let mut upcall_errs = 0u32;
    let mut timeouts = 0u32;
    let mut min_us = u32::MAX;
    let mut max_us = 0u32;
    let mut kept = 0usize;

    loop {
        let now = Alarm::get_ticks().unwrap_or(0);
        if ms(now.wrapping_sub(ready_at), hz) >= RUN_MS {
            break;
        }
        attempts += 1;

        let frame = frame_step(0, 0, BAND_W, BAND_H, timeout_ticks);
        let t0 = Alarm::get_ticks().unwrap_or(0);
        let write = write_step(&band, timeout_ticks);
        let t1 = Alarm::get_ticks().unwrap_or(0);

        // The write's own latency, not the pair's: the frame call is the one
        // that would be queued behind app A's fill, and folding the two
        // together would hide which of them waited.
        let elapsed = us(t0, t1, hz);
        min_us = min_us.min(elapsed);
        max_us = max_us.max(elapsed);

        match write.outcome {
            Outcome::Refused => refused += 1,
            Outcome::Timeout => timeouts += 1,
            Outcome::Upcall(0, _, _) => clean += 1,
            Outcome::Upcall(..) => upcall_errs += 1,
            Outcome::Setup(_) => {}
        }

        // Printing here costs a UART line and stops the contention for its
        // duration, which is acceptable only because it is the finding.
        if (!write.is_clean() || !frame.is_clean()) && kept < KEEP {
            kept += 1;
            let _ = writeln!(
                console,
                "busy_probe: ANOMALY #{attempts} at t={} ms -- frame {frame} | \
                 write {write}\r",
                ms(now, hz)
            );
        }
    }

    let _ = writeln!(
        console,
        "busy_probe: {attempts} writes -- {clean} ok, {refused} refused, \
         {upcall_errs} upcall errors, {timeouts} timeouts\r\n\
         \x20 write latency min {min_us} us, max {max_us} us\r"
    );
    if refused == 0 && upcall_errs == 0 && timeouts == 0 {
        let _ = writeln!(
            console,
            "busy_probe: no write ever met a refusal. NOT OBSERVED under this \
             contention --\r\n\
             \x20 not a proof it cannot happen. Check the max latency above is \
             near a full-screen\r\n\
             \x20 fill (~40 ms); if it is not, app A was not loading the driver \
             and this run is void.\r"
        );
    }
}

fn raw(c: &CommandReturn) -> (u32, u32, u32, u32) {
    let (variant, r1, r2, r3) = c.raw_values();
    (variant.into(), r1, r2, r3)
}

/// TRD 104: variants below 128 are failures, 128 and above successes. A
/// failure means no upcall was scheduled, so there is nothing to wait for.
fn scheduled(c: &CommandReturn) -> bool {
    u32::from(c.return_variant()) >= 128
}

fn ms(ticks: u32, hz: u32) -> u32 {
    ((ticks as u64 * 1_000) / hz as u64) as u32
}

fn us(a: u32, b: u32, hz: u32) -> u32 {
    ((b.wrapping_sub(a) as u64 * 1_000_000) / hz as u64) as u32
}

/// Registers the screen upcall and the deadline's upcall.
///
/// A macro rather than a function because it must expand inside the caller's
/// `share::scope` closure: a helper taking the handles and the cells would
/// unify the share lifetime with the cells', and `AllowRo` is invariant over
/// that lifetime, so the handle would escape the body it is valid in.
macro_rules! subscribe_both {
    ($screen_sub:expr, $called:expr, $alarm_sub:expr, $fired:expr) => {
        if let Err(e) =
            TockSyscalls::subscribe::<_, _, DefaultConfig, SCREEN, SCREEN_CB>($screen_sub, $called)
        {
            return Step {
                cmd: (0, 0, 0, 0),
                outcome: Outcome::Setup(e),
            };
        }
        if let Err(e) =
            TockSyscalls::subscribe::<_, _, DefaultConfig, ALARM, ALARM_CB>($alarm_sub, $fired)
        {
            return Step {
                cmd: (0, 0, 0, 0),
                outcome: Outcome::Setup(e),
            };
        }
    };
}

fn frame_step(x: u32, y: u32, w: u32, h: u32, timeout_ticks: u32) -> Step {
    let called: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
    let fired: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
    let data1 = ((x & 0xFFFF) << 16) | (y & 0xFFFF);
    let data2 = ((w & 0xFFFF) << 16) | (h & 0xFFFF);

    share::scope::<
        (
            Subscribe<_, SCREEN, SCREEN_CB>,
            Subscribe<_, ALARM, ALARM_CB>,
        ),
        _,
        _,
    >(|handle| {
        let (screen_sub, alarm_sub) = handle.split();
        subscribe_both!(screen_sub, &called, alarm_sub, &fired);
        let cr = TockSyscalls::command(SCREEN, SET_WRITE_FRAME, data1, data2);
        finish(cr, &called, &fired, timeout_ticks)
    })
}

fn write_step(pixels: &[u8], timeout_ticks: u32) -> Step {
    let called: Cell<Option<(u32, u32, u32)>> = Cell::new(None);
    let fired: Cell<Option<(u32, u32, u32)>> = Cell::new(None);

    share::scope::<
        (
            AllowRo<_, SCREEN, WRITE_BUF>,
            Subscribe<_, SCREEN, SCREEN_CB>,
            Subscribe<_, ALARM, ALARM_CB>,
        ),
        _,
        _,
    >(|handle| {
        let (allow, screen_sub, alarm_sub) = handle.split();
        if let Err(e) = TockSyscalls::allow_ro::<DefaultConfig, SCREEN, WRITE_BUF>(allow, pixels) {
            return Step {
                cmd: (0, 0, 0, 0),
                outcome: Outcome::Setup(e),
            };
        }
        subscribe_both!(screen_sub, &called, alarm_sub, &fired);
        let cr = TockSyscalls::command(SCREEN, WRITE, pixels.len() as u32, 0);
        finish(cr, &called, &fired, timeout_ticks)
    })
}

/// Classifies the command, and waits for its upcall only if one is owed.
fn finish(
    cr: CommandReturn,
    called: &Cell<Option<(u32, u32, u32)>>,
    fired: &Cell<Option<(u32, u32, u32)>>,
    timeout_ticks: u32,
) -> Step {
    let cmd = raw(&cr);
    if !scheduled(&cr) {
        return Step {
            cmd,
            outcome: Outcome::Refused,
        };
    }
    if let Err(e) = TockSyscalls::command(ALARM, ALARM_SET_RELATIVE, timeout_ticks, 0)
        .to_result::<u32, ErrorCode>()
    {
        return Step {
            cmd,
            outcome: Outcome::Setup(e),
        };
    }
    loop {
        TockSyscalls::yield_wait();
        if let Some((a, b, c)) = called.get() {
            let _ = TockSyscalls::command(ALARM, ALARM_STOP, 0, 0);
            return Step {
                cmd,
                outcome: Outcome::Upcall(a, b, c),
            };
        }
        if fired.get().is_some() {
            return Step {
                cmd,
                outcome: Outcome::Timeout,
            };
        }
    }
}
