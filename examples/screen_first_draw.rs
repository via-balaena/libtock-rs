//! Does the screen driver survive being called before the panel is ready?
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=screen_first_draw
//! ```
//!
//! Built to settle one question for the kernel side: `hil::screen::Screen::write`
//! used to take a `SubSliceMut<'static, u8>` and return `Result<(), ErrorCode>`,
//! so a refusal **consumed the buffer and gave nothing back**. The syscall
//! driver holds exactly one, cannot make another, and has no way to ask for it
//! back — so the predicted failure is that a single refused write kills the
//! screen for every process, permanently.
//!
//! The kit's ST7796 answers BUSY for about 1,239 ms after boot while its init
//! sequence runs, and nothing surfaces readiness to an app. So an app that
//! draws immediately walks straight into that window, which is what this one
//! does.
//!
//! **This app does not know which kernel it is running on and is not told.**
//! It reports the sequence of raw syscall answers over time; which prediction
//! that matches is read off afterwards.
//!
//! # Why the raw registers and not `Screen::write`
//!
//! `libtock::display::Screen` collapses the upcall status through
//! `try_into().unwrap_or(ErrorCode::Fail)`, so an unrecognised status arrives
//! as `Fail` and is indistinguishable from a real `Fail`. Here the command's
//! four return registers and the upcall's three arguments are printed as
//! integers, untranslated.
//!
//! # Why there is a timeout, which is the part that matters
//!
//! A destroyed buffer most likely shows up as **no upcall at all**, not as an
//! error code: `fill_next_buffer_for_write` has nothing to fill, so nothing
//! completes and nothing is scheduled. Every `Screen::*` wrapper answers that
//! by spinning in `yield_wait` forever. The app would hang on attempt 1 and
//! emit a single line — and a hang, a crash and a silent failure all look the
//! same from the console.
//!
//! So each attempt waits with a deadline, and reports three distinct outcomes:
//!
//! * `refused` — the command returned a failure variant. **No upcall was
//!   scheduled**, so this waits for nothing; that is why the variant is
//!   checked before waiting rather than after.
//! * `up=(..)` — an upcall arrived. Its raw arguments; `up=(0,..)` is success.
//! * `TIMEOUT` — the command was accepted and nothing ever completed it.
//!
//! The deadline is 2,000 ms rather than the 100 ms retry interval because a
//! kernel that *queues* the command and serves it from `screen_is_ready`
//! answers at ~1,240 ms, and that is a success, not a hang. A 100 ms deadline
//! would call the working path a failure.
//!
//! # Both calls are issued every attempt, even after the frame is refused
//!
//! The buffer belongs to the **write** path. Gating the write on
//! `set_write_frame` succeeding would skip it exactly when the panel is busy,
//! which is the only window where the bug can be triggered, so the run would
//! be vacuous. The frame call is still worth issuing first: it is the same
//! capsule with no buffer involved, so a difference between the two is
//! informative.
//!
//! # Reading the timestamps
//!
//! `t=` is when the attempt **started** — kernel tick zero, not process start.
//! The two `+` fields are how long each of the two calls took, clocked between
//! them and before anything is printed, because the console write blocks for
//! the UART and would otherwise be inside the measurement.
//!
//! So `t=34 frame +1224 | write +2` says the frame call was issued 34 ms after
//! tick zero and did not answer for 1,224 ms, and the write that followed it
//! took 2. **That is one call absorbing the readiness gap, not both** — which
//! is the whole reason the two are clocked apart. A single per-attempt
//! timestamp cannot tell those apart, and the obvious reading of one, that the
//! write was accepted early and completed late, is wrong.
//!
//! This is still not the instrument for the gap itself. It reports when *this
//! app* stopped waiting, which is the gap minus however late the process
//! started. `screen_ready.rs` is the one that measures it: one call, clock at
//! both ends.
//!
//! # What a timeout does to the lines after it
//!
//! Nothing cancels an accepted command. After a timeout, an upcall may still
//! be outstanding, and the next attempt's BUSY could mean "panel not ready" or
//! "a call is already running" — two different meanings with one code. Lines
//! after the first timeout are prefixed `!` for that reason; they are still
//! data, but they are no longer clean.

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::allow_ro::AllowRo;
use libtock_platform::share;
use libtock_platform::subscribe::Subscribe;
use libtock_platform::{CommandReturn, DefaultConfig, ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x4000}

const SCREEN: u32 = 0x9_0001;
const SCREEN_EXISTS: u32 = 0;
const GET_RESOLUTION: u32 = 23;
const SET_WRITE_FRAME: u32 = 100;
const WRITE: u32 = 200;
const FILL: u32 = 300;
const SCREEN_CB: u32 = 0;
const WRITE_BUF: u32 = 0;

const ALARM: u32 = 0x0;
const ALARM_STOP: u32 = 3;
const ALARM_SET_RELATIVE: u32 = 5;
const ALARM_CB: u32 = 0;

/// The band to draw: full width, a few rows. 3,840 px at RGB565 is 7,680
/// bytes, inside the capsule's 12,800-byte buffer, so it is **one chunk**.
/// Chunked writes re-enter `fill_next_buffer_for_write` per chunk and would
/// add a second way to fail; this run is about the first one.
const BAND_W: u32 = 480;
const BAND_H: u32 = 8;
const BAND_BYTES: usize = (BAND_W * BAND_H) as usize * 2;

/// Green, and full-screen green on success. Unmistakable against an
/// uninitialised panel.
const GREEN: u16 = 0x07E0;

/// How long to wait for an upcall on an accepted command. See the header: it
/// must exceed the ~1,240 ms readiness gap or the queued path reads as a hang.
const UPCALL_TIMEOUT_MS: u32 = 2_000;

/// Retry cadence, measured from the start of each attempt, so an attempt that
/// blocks longer than this simply retries immediately.
const RETRY_MS: u32 = 100;

/// Total run, from the process's first clock read. Eight seconds rather than
/// five so that a first attempt which blocks for the readiness gap still
/// leaves about five seconds of retries behind it.
const RUN_MS: u32 = 8_000;

/// Bounds the console output if something retries very fast.
const MAX_ATTEMPTS: u32 = 64;

/// What happened to one screen syscall.
enum Outcome {
    /// Command returned a failure variant; no upcall was scheduled.
    Refused,
    /// An upcall arrived, with these three raw arguments.
    Upcall(u32, u32, u32),
    /// Command was accepted and no upcall arrived before the deadline.
    Timeout,
    /// subscribe/allow/alarm failed, so the attempt never ran. Not a result.
    Setup(ErrorCode),
}

/// One syscall: its four raw return registers, and what followed.
struct Step {
    cmd: (u32, u32, u32, u32),
    outcome: Outcome,
}

impl Step {
    /// True only for an upcall reporting status 0.
    fn succeeded(&self) -> bool {
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
            let _ = writeln!(console, "first_draw: no usable alarm frequency\r");
            return;
        }
    };
    let t_start = Alarm::get_ticks().unwrap_or(0);

    // The first screen syscall of the run, on the timeline like everything
    // else. `EXISTS` is answered by the capsule without touching the driver,
    // so it says nothing about readiness -- only whether the board wired the
    // panel to userspace at all.
    let exists = TockSyscalls::command(SCREEN, SCREEN_EXISTS, 0, 0);
    let exists_raw = raw(&exists);
    let res = TockSyscalls::command(SCREEN, GET_RESOLUTION, 0, 0);
    let res_raw = raw(&res);
    let (w, h) = res.get_success_2_u32().unwrap_or((BAND_W, 320));

    let _ = writeln!(
        console,
        "first_draw: alarm {hz} Hz, t is ms since the kernel's tick zero\r\n\
         \x20 exists cmd={exists_raw:?}  get_resolution cmd={res_raw:?} -> {w}x{h}\r\n\
         \x20 cmd=(r0,r1,r2,r3) raw; r0<128 is a failure variant and r1 is then \
         the ErrorCode\r\n\
         \x20 up=(a0,a1,a2) raw upcall args; up=(0,..) is success\r\n\
         \x20 band {BAND_W}x{BAND_H}, deadline {UPCALL_TIMEOUT_MS} ms, retry \
         {RETRY_MS} ms, run {RUN_MS} ms\r"
    );
    if !scheduled(&exists) {
        let _ = writeln!(
            console,
            "first_draw: no screen driver at 0x90001 -- build the board with \
             kit_display\r"
        );
        return;
    }

    let mut band = [0u8; BAND_BYTES];
    for pair in band.chunks_exact_mut(2) {
        pair.copy_from_slice(&GREEN.to_be_bytes());
    }
    let band_w = w.min(BAND_W);
    let band_h = h.min(BAND_H);
    let band_bytes = (band_w * band_h) as usize * 2;
    let timeout_ticks = ((UPCALL_TIMEOUT_MS as u64 * hz as u64) / 1_000) as u32;

    let mut attempt = 0u32;
    let mut refused = 0u32;
    let mut timeouts = 0u32;
    let mut failed_upcalls = 0u32;
    let mut succeeded = 0u32;
    let mut first_success_ms = None;
    let mut poisoned = false;

    loop {
        let now = Alarm::get_ticks().unwrap_or(0);
        if ms(now.wrapping_sub(t_start), hz) >= RUN_MS || attempt >= MAX_ATTEMPTS {
            break;
        }
        attempt += 1;
        let attempt_start = now;

        // Clocked between and after the two calls, and BEFORE anything is
        // printed. The console write below blocks for the UART -- about 8 ms
        // for a line this long at 115200 -- so a clock read taken after it
        // would fold the transmission into the measurement. An earlier version
        // timestamped the first success after printing, which inflated it by
        // exactly that.
        let frame = frame_step(0, 0, band_w, band_h, timeout_ticks);
        let after_frame = Alarm::get_ticks().unwrap_or(0);
        let write = write_step(&band[..band_bytes], timeout_ticks);
        let after_write = Alarm::get_ticks().unwrap_or(0);

        // Tally the write, which is the call that carries the buffer.
        match write.outcome {
            Outcome::Refused => refused += 1,
            Outcome::Timeout => timeouts += 1,
            Outcome::Upcall(0, _, _) => succeeded += 1,
            Outcome::Upcall(..) => failed_upcalls += 1,
            Outcome::Setup(_) => {}
        }

        let mark = if poisoned { "!" } else { " " };
        let _ = writeln!(
            console,
            "{mark}t={:>6} ms #{attempt:<2} frame +{:<5} {frame} | write +{:<5} {write}\r",
            ms(now, hz),
            ms(after_frame.wrapping_sub(now), hz),
            ms(after_write.wrapping_sub(after_frame), hz)
        );

        if matches!(frame.outcome, Outcome::Timeout) || matches!(write.outcome, Outcome::Timeout) {
            poisoned = true;
        }

        if write.succeeded() && first_success_ms.is_none() {
            first_success_ms = Some(ms(after_write, hz));
            // The second channel: the panel itself. The band above already
            // painted; this makes it the whole screen so it cannot be missed
            // or mistaken for leftover content.
            let full = frame_step(0, 0, w, h, timeout_ticks);
            let fill = fill_step(GREEN, timeout_ticks);
            let _ = writeln!(
                console,
                " t={:>6} ms FIRST DRAW -- painting full screen: frame {full} | \
                 fill {fill}\r",
                ms(after_write, hz)
            );
        }

        // Cadence from the start of the attempt, so a long attempt does not
        // push the whole schedule out.
        let spent = ms(
            Alarm::get_ticks().unwrap_or(0).wrapping_sub(attempt_start),
            hz,
        );
        if spent < RETRY_MS {
            let _ = Alarm::sleep_for(Milliseconds(RETRY_MS - spent));
        }
    }

    let _ = writeln!(
        console,
        "first_draw: {attempt} attempts -- {succeeded} ok, {refused} refused, \
         {failed_upcalls} upcall errors, {timeouts} timeouts\r"
    );
    match first_success_ms {
        Some(t) => {
            let _ = writeln!(
                console,
                "first_draw: first successful write at t={t} ms; the driver \
                 recovered\r"
            );
        }
        None => {
            let _ = writeln!(
                console,
                "first_draw: no write ever succeeded in {RUN_MS} ms\r"
            );
        }
    }
    if poisoned {
        let _ = writeln!(
            console,
            "first_draw: a command was accepted and never completed, so lines \
             marked ! ran with an\r\n\
             \x20 outstanding call and their BUSY is ambiguous. Treat the run as \
             ending at the first !\r"
        );
    }
}

/// The command's four return registers, untranslated.
fn raw(c: &CommandReturn) -> (u32, u32, u32, u32) {
    let (variant, r1, r2, r3) = c.raw_values();
    (variant.into(), r1, r2, r3)
}

/// Whether the command was accepted, and therefore whether an upcall is owed.
///
/// TRD 104: return variants below 128 are failures, 128 and above successes.
/// Compared numerically rather than by listing the success variants, so a
/// variant this app has never seen is still classified rather than mistaken
/// for a failure.
fn scheduled(c: &CommandReturn) -> bool {
    u32::from(c.return_variant()) >= 128
}

fn ms(ticks: u32, hz: u32) -> u32 {
    ((ticks as u64 * 1_000) / hz as u64) as u32
}

/// Registers the screen upcall and the deadline's upcall, in that order.
///
/// A macro rather than a function because it must expand *inside* the caller's
/// `share::scope` closure. A helper taking the two handles and the two cells
/// would unify the share lifetime with the cells' -- which outlive the closure
/// -- and `AllowRo` is invariant over that lifetime, so the handle would be
/// escaping the body it is only valid in.
///
/// The alarm is subscribed but not armed here. An unarmed alarm delivers
/// nothing, so a command that comes back refused costs no timer at all.
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

/// `set_write_frame`, with a deadline on its upcall.
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

/// `write`, the call that hands the capsule its buffer.
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

/// `fill`, used only for the full-screen paint after the first success.
fn fill_step(colour: u16, timeout_ticks: u32) -> Step {
    let pixel = colour.to_be_bytes();
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
        if let Err(e) = TockSyscalls::allow_ro::<DefaultConfig, SCREEN, WRITE_BUF>(allow, &pixel) {
            return Step {
                cmd: (0, 0, 0, 0),
                outcome: Outcome::Setup(e),
            };
        }
        subscribe_both!(screen_sub, &called, alarm_sub, &fired);
        let cr = TockSyscalls::command(SCREEN, FILL, 0, 0);
        finish(cr, &called, &fired, timeout_ticks)
    })
}

/// Classifies the command, and waits for its upcall only if one is owed.
///
/// A failure variant means the capsule scheduled nothing, so waiting would
/// block until some unrelated upcall arrived or the deadline expired -- and
/// would report `TIMEOUT` for a command that was answered immediately. That is
/// why the variant decides, not the wait.
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
    // `yield_wait` returns on *any* upcall -- the console's own completions
    // included -- so both cells are re-checked on every wake rather than
    // assuming which one fired. The screen is checked first, so an upcall that
    // lands in the same window as the deadline is reported as the answer it is.
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
