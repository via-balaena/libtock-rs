//! Asks whether `stop_scan` can stop a scan, by calling it in four states and
//! comparing the answers. **It cannot**, and this app is why that is a
//! measurement rather than a reading.
//!
//! # Measured
//!
//! Pico 2 W, kernel `0b229f354`, 2026-09-10, run by the kernel session.
//! 48 networks seen.
//!
//! ```text
//! A idle              command 8 -> Ok(())     expected not BUSY  ok
//! B scan starting     command 8 -> Err(BUSY)  expected BUSY      ok
//! C scan running      command 8 -> Err(BUSY)  expected BUSY      ok
//! D after terminator  command 8 -> Ok(())     expected not BUSY  ok
//! ```
//!
//! All four matched. A and D are what make it a finding rather than a
//! suspicion: the command works on this radio, it simply cannot do the one
//! thing it is named for.
//!
//! # The claim under test
//!
//! Command 8 is documented as "stop scanning". Reading the kernel says it can
//! only succeed when there is no scan to stop. Every device entry point goes
//! through `init_tasks`, which refuses with `BUSY` whenever
//! `!self.ioctl_tasks.is_empty() || self.pending.is_some()`
//! (`cyw4343/driver.rs:236-237`), and `stop_scan` propagates that with `?`
//! before it queues anything or changes any state (`:670-678`). `pending` stays
//! set for the whole life of a scan, cleared only by a scan-done event
//! (`:470-476`).
//!
//! That was noticed while correcting a different claim — an earlier version of
//! `wifi_scan_exit.rs` said `stop_scan` merely narrowed the window for an
//! exiting app, a race, and following `init_tasks` one call further showed
//! there is no window. It left this question behind, which nobody had asked.
//!
//! # What this app cannot answer
//!
//! **Whether a stop that was allowed through would actually end the scan.** The
//! obvious next phase — issue the stop at C, then watch for a terminator —
//! cannot work, and it is worth saying so here so nobody builds it. At C the
//! command returns `BUSY` from `init_tasks` *before* `stop_scan` queues an
//! ioctl or sets any state, so the radio is never told anything. The terminator
//! that follows is the scan ending by itself, which would have arrived
//! regardless, and observing it says nothing about stopping.
//!
//! Since `pending` is cleared only by a scan-done event (`:470-476`), a fix
//! that relaxes the guard also needs to know whether the radio answers a stop
//! with one. That question cannot be reached from userspace on a kernel whose
//! guard still refuses the command: it needs the guard relaxed first, which
//! makes it a kernel spike rather than an app.
//!
//! # Why four calls and not one
//!
//! A single `BUSY` during a scan proves less than it looks. It is also what a
//! wedged driver, a radio that never came up, or a capsule that had stopped
//! accepting command 8 at all would return. So the same command is issued in
//! four states and the *differences* carry the result:
//!
//! ```text
//! A  idle, before any scan      expect: not BUSY
//! B  immediately after command 7  expect: BUSY
//! C  after a result has arrived   expect: BUSY   <- the load-bearing one
//! D  after the terminator         expect: not BUSY
//! ```
//!
//! A and D are the controls. If command 8 were simply broken, or the radio
//! never usable, they would fail too and the run says so instead of reporting a
//! finding.
//!
//! **C is the one that matters**, and its precondition is observable rather
//! than timed. B can be `BUSY` for a boring reason: the `start_scan` ioctl is
//! still in flight, so `ioctl_tasks` is non-empty and `pending` has not been set
//! yet. Waiting for a scan *result* removes that ambiguity — a result can only
//! arrive after the ioctl queue drained and `update_task` set `pending`
//! (`cyw4343/driver.rs:536-541`). So at C the only thing that can still be
//! refusing the command is `pending`, which is the actual claim.
//!
//! # Buffer
//!
//! A buffer is shared at rw_allow 1 even though no SSID is printed. Since
//! `0b229f354` the kernel drops a scan result that would reach the process with
//! nothing written, so without a buffer this app would see no results at all,
//! phase C would never reach its precondition, and the run would time out
//! looking like a dead radio.

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::subscribe::AnyId;
use libtock_platform::{share, AllowRw, DefaultConfig, ErrorCode, Subscribe, Syscalls, Upcall};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

const DRIVER_NUM: u32 = 0x30008;

mod command {
    pub const EXISTS: u32 = 0;
    pub const INIT: u32 = 1;
    pub const SCAN: u32 = 7;
    pub const STOP_SCAN: u32 = 8;
}

mod upcall {
    pub const INIT: u32 = 0;
    pub const STOP_SCAN: u32 = 6;
    pub const SCAN_RES: u32 = 7;
}

mod rw_allow {
    pub const SCAN_SSID: u32 = 1;
}

const POLL_MS: u32 = 20;
const INIT_TIMEOUT_MS: u32 = 10_000;
const STOP_TIMEOUT_MS: u32 = 5_000;
const RESULT_TIMEOUT_MS: u32 = 10_000;
const SCAN_TIMEOUT_MS: u32 = 20_000;

/// Counts scan upcalls. Lengths do not matter here; arrival does.
#[derive(Default)]
struct ScanSink {
    seen: Cell<u32>,
    done: Cell<bool>,
}

impl Upcall<AnyId> for ScanSink {
    fn upcall(&self, len: u32, _: u32, _: u32) {
        if len == 0 {
            self.done.set(true);
        } else {
            self.seen.set(self.seen.get() + 1);
        }
    }
}

fn wait_until(timeout_ms: u32, ready: impl Fn() -> bool) -> u32 {
    let mut waited = 0;
    while waited < timeout_ms {
        if ready() {
            return waited;
        }
        if Alarm::sleep_for(Milliseconds(POLL_MS)).is_err() {
            return waited;
        }
        waited += POLL_MS;
    }
    waited
}

/// What one call to command 8 returned, and whether that matched expectation.
struct Phase {
    name: &'static str,
    result: Result<(), ErrorCode>,
    expected_busy: bool,
}

impl Phase {
    fn holds(&self) -> bool {
        matches!(self.result, Err(ErrorCode::Busy)) == self.expected_busy
    }
}

fn main() {
    let mut console = Console::writer();

    if TockSyscalls::command(DRIVER_NUM, command::EXISTS, 0, 0)
        .to_result::<(), ErrorCode>()
        .is_err()
    {
        let _ = writeln!(console, "wifi_stop_scan: no driver at {DRIVER_NUM:#x}");
        return;
    }

    if !init(&mut console) {
        return;
    }

    let sink = ScanSink::default();
    let stopped: Cell<Option<(u32,)>> = Cell::new(None);
    let mut ssid = [0u8; 32];
    let mut phases: [Option<Phase>; 4] = [None, None, None, None];

    let outcome = share::scope::<
        (
            AllowRw<_, DRIVER_NUM, { rw_allow::SCAN_SSID }>,
            Subscribe<_, DRIVER_NUM, { upcall::STOP_SCAN }>,
            Subscribe<_, DRIVER_NUM, { upcall::SCAN_RES }>,
        ),
        _,
        _,
    >(|handle| {
        let (allow_rw, sub_stop, sub_scan) = handle.split();

        TockSyscalls::allow_rw::<DefaultConfig, DRIVER_NUM, { rw_allow::SCAN_SSID }>(
            allow_rw, &mut ssid,
        )?;
        TockSyscalls::subscribe::<_, _, DefaultConfig, DRIVER_NUM, { upcall::STOP_SCAN }>(
            sub_stop, &stopped,
        )?;
        TockSyscalls::subscribe::<_, _, DefaultConfig, DRIVER_NUM, { upcall::SCAN_RES }>(
            sub_scan, &sink,
        )?;

        // A: idle. Nothing is scanning, so this is the control that says
        // command 8 works at all on this radio.
        stopped.set(None);
        phases[0] = Some(Phase {
            name: "A idle",
            result: stop_scan(),
            expected_busy: false,
        });
        // Let it drain before starting the scan. A queued ioctl left in flight
        // would make the next command BUSY for the wrong reason.
        if phases[0].as_ref().is_some_and(|p| p.result.is_ok()) {
            wait_until(STOP_TIMEOUT_MS, || stopped.get().is_some());
        }

        // B: the instant after the scan command returns. `pending` is probably
        // not set yet and the start_scan ioctl is in flight, so a BUSY here is
        // expected but weakly informative.
        TockSyscalls::command(DRIVER_NUM, command::SCAN, 0, 0).to_result::<(), ErrorCode>()?;
        phases[1] = Some(Phase {
            name: "B scan starting",
            result: stop_scan(),
            expected_busy: true,
        });

        // C: a result has arrived, which can only happen after the ioctl queue
        // drained and `pending` was set. Anything refusing command 8 now is
        // `pending`, which is the claim.
        let waited = wait_until(RESULT_TIMEOUT_MS, || sink.seen.get() > 0 || sink.done.get());
        if sink.seen.get() > 0 {
            phases[2] = Some(Phase {
                name: "C scan running",
                result: stop_scan(),
                expected_busy: true,
            });
        } else {
            let _ = writeln!(
                console,
                "wifi_stop_scan: no scan result within {waited} ms — phase C not reached"
            );
        }

        // D: after the scan ends on its own. The other control: command 8 should
        // work again once there is nothing to stop.
        wait_until(SCAN_TIMEOUT_MS, || sink.done.get());
        if sink.done.get() {
            stopped.set(None);
            phases[3] = Some(Phase {
                name: "D after terminator",
                result: stop_scan(),
                expected_busy: false,
            });
            wait_until(STOP_TIMEOUT_MS, || stopped.get().is_some());
        } else {
            let _ = writeln!(
                console,
                "wifi_stop_scan: scan never terminated — phase D not reached"
            );
        }

        Ok::<(), ErrorCode>(())
    });

    if let Err(e) = outcome {
        let _ = writeln!(console, "wifi_stop_scan: could not run: {e:?}");
        return;
    }

    report(&mut console, &phases, sink.seen.get());
}

fn stop_scan() -> Result<(), ErrorCode> {
    TockSyscalls::command(DRIVER_NUM, command::STOP_SCAN, 0, 0).to_result::<(), ErrorCode>()
}

fn report(console: &mut impl Write, phases: &[Option<Phase>; 4], seen: u32) {
    let mut reached = 0;
    let mut held = 0;

    for phase in phases.iter().flatten() {
        reached += 1;
        if phase.holds() {
            held += 1;
        }
        let _ = writeln!(
            console,
            "wifi_stop_scan: {:<19} command 8 -> {:?}  expected {}  {}",
            phase.name,
            phase.result,
            if phase.expected_busy {
                "BUSY"
            } else {
                "not BUSY"
            },
            if phase.holds() { "ok" } else { "MISMATCH" }
        );
    }

    let _ = writeln!(console, "wifi_stop_scan: {seen} networks seen");

    if reached != 4 {
        let _ = writeln!(
            console,
            "wifi_stop_scan: INCONCLUSIVE — only {reached} of 4 phases reached"
        );
        return;
    }

    if held == 4 {
        // The controls fired, so this is about the scan and not about the
        // command being broken.
        let _ = writeln!(
            console,
            "wifi_stop_scan: CONFIRMED — command 8 succeeds idle and after the scan, and \
             returns BUSY while one is running. A scan cannot be stopped by the command \
             named for it."
        );
    } else {
        let _ = writeln!(
            console,
            "wifi_stop_scan: NOT CONFIRMED — {held} of 4 phases matched; read the rows above, \
             the reading this app was written from is wrong somewhere"
        );
    }
}

/// Command 1, then upcall 0. Returns whether the radio came up.
fn init(console: &mut impl Write) -> bool {
    let status: Cell<Option<(u32,)>> = Cell::new(None);

    let started = share::scope(|subscribe| {
        TockSyscalls::subscribe::<_, _, DefaultConfig, DRIVER_NUM, { upcall::INIT }>(
            subscribe, &status,
        )?;

        TockSyscalls::command(DRIVER_NUM, command::INIT, 0, 0).to_result::<(), ErrorCode>()?;

        Ok::<u32, ErrorCode>(wait_until(INIT_TIMEOUT_MS, || status.get().is_some()))
    });

    let waited = match started {
        Ok(waited) => waited,
        Err(e) => {
            let _ = writeln!(console, "wifi_stop_scan: init could not start: {e:?}");
            return false;
        }
    };

    match status.get() {
        Some((0,)) => {
            let _ = writeln!(console, "wifi_stop_scan: init ok after {waited} ms");
            true
        }
        Some((status,)) => {
            let _ = writeln!(console, "wifi_stop_scan: init returned status {status}");
            false
        }
        None => {
            let _ = writeln!(
                console,
                "wifi_stop_scan: init did not call back within {INIT_TIMEOUT_MS} ms"
            );
            false
        }
    }
}
