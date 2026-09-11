//! Starts a WiFi scan and terminates the process while results are still
//! arriving. This used to panic the kernel. **It is now a regression test, and
//! the passing outcome is that nothing happens.**
//!
//! Read that before flashing it: on a fixed kernel this app prints three lines
//! and exits, and the bench is not broken. On a kernel without the fix it takes
//! the board down. Which of those you get is the measurement.
//!
//! # Measured
//!
//! Both on a Pico 2 W, 2026-09-10, run by the kernel session.
//!
//! **Before the fix**, kernel `839a242f3`:
//!
//! ```text
//! wifi_scan_exit: init ok after 380 ms
//! wifi_scan_exit: 2 networks after 60 ms — terminating now with the scan
//!                 still running. Anything below this line is the kernel's.
//!
//! panicked at capsules/extra/src/wifi/driver.rs:303:18:
//! called `Result::unwrap()` on an `Err` value: InactiveApp
//! ```
//!
//! The predicted line and the predicted error variant, both exact. 4092 bytes
//! of panic dump.
//!
//! **After the fix**, `0f44a11a7`, this app unmodified:
//!
//! ```text
//! wifi_scan_exit: 2 networks after 40 ms — terminating now with the scan
//!                 still running.
//!
//! tock$ list
//!  0  Unique  wifi_scan_exit  0  194  0  0/5  Terminated
//! tock$ status
//!  Total processes: 1 / Active processes: 0
//! ```
//!
//! 215 bytes total and nothing below that line. The `list` and `status` matter
//! more than the silence: a kernel that had died quietly would also print
//! nothing, so the check is that it still services the console and answers
//! afterwards, with the app Terminated and its grants released.
//!
//! # Why it mattered
//!
//! The kernel's own dump assigned the app no fault: `Completion Code: 0`,
//! `Last Syscall: Exit { which: 0, completion_code: 0 }`, and "No Cortex-M
//! faults detected". The process exited cleanly, which processes are allowed to
//! do. So on an unfixed kernel any process permitted to use driver `0x30008`
//! could halt the board by starting a scan and leaving, and **no userspace
//! mitigation existed**: an app cannot decline to be terminated, and calling
//! `stop_scan` on the way out does not help.
//!
//! That last part is checkable rather than hopeful, and it is worse than a
//! race: **`stop_scan` cannot stop a scan at all.** Every device entry point
//! that would touch the radio goes through `init_tasks`, which refuses with
//! `BUSY` whenever a scan is outstanding — `!self.ioctl_tasks.is_empty() ||
//! self.pending.is_some()` (`cyw4343/driver.rs:236-237`) — and `stop_scan`
//! propagates that with `?` before it queues anything or changes any state
//! (`driver.rs:670-678`). `pending` stays set until a scan-done event clears it
//! (`driver.rs:470-476`), so for the whole life of a scan, command 8 returns
//! `BUSY` and the scan runs on.
//!
//! An app that tries to be tidy on the way out therefore gets an error and
//! exits anyway. This is not a window it might miss; there is no window.
//!
//! That is what made it a denial of service from userspace rather than merely an
//! unwrap that can fail. **This part is unfixed and still current:** `0f44a11a7`
//! stops the panic, not the scan, so command 8 still cannot stop one. Nothing
//! here depends on it — this app never calls `stop_scan` — but a reader who
//! takes the `BUSY` behaviour as history rather than present tense will be
//! wrong.
//!
//! # What it is not
//!
//! The blast radius is worth stating, because "the capsule keeps talking to a
//! dead process" invites two worse readings that are both false.
//!
//! **It cannot misdeliver to a later process.** A stale `ProcessId` can never
//! match a restarted or newly loaded one: `ProcessId::eq` compares the
//! identifier alone and ignores the array index (`kernel/src/process.rs:97-101`),
//! identifiers come from a monotonic counter (`kernel.rs:300-302`), and
//! `ProcessStandard::reset` mints a fresh one while keeping the old index —
//! its own comment says it does so "to invalidate any stored `ProcessId`s that
//! point to the old version of the process" (`process_standard.rs:2370-2379`).
//! So `get_process`'s filter cannot match, and the result is dropped rather than
//! handed to whoever came next.
//!
//! **It does not leave the radio scanning forever.** The scan self-terminates:
//! the non-partial `EscanResult` clears `self.pending` after calling
//! `scan_done` (`cyw4343/driver.rs:470-476`).
//!
//! **A later app cannot be handed the dead one's terminator.** The capsule keeps
//! a single `process_id` and sets it unconditionally on command 7
//! (`wifi/driver.rs:238-246`), which looks like it should let a second app take
//! over delivery and receive a terminator belonging to the first. It cannot,
//! because every command that assigns `process_id` — 3 through 8 — first calls a
//! device method guarded by `init_tasks`, which returns `BUSY` while a scan is
//! pending, and `wifi/driver.rs` assigns `process_id` only when that call
//! returned `Ok`. Command 1 returns `ALREADY` on an initialised radio; command 2
//! never assigns. And the drain path sets `pending` *before* `tasks_done` clears
//! the task list (`cyw4343/driver.rs:536-541`), so there is no instant where
//! both tests are false. Delivery during a scan always reaches the process that
//! started it.
//!
//! That is a reading, not a run, and it bounds this defect rather than clearing
//! the capsule: it says the predicted hijack is blocked on these paths, not that
//! every interleaving of a multi-process capsule with one delivery target is
//! safe.
//!
//! So once the `Err` is caught instead of unwrapped, what remains is bounded and
//! harmless — for the rest of one scan, results are offered to a terminated
//! process, `grants.enter` returns `Err(InactiveApp)`, and they are dropped.
//!
//! # The mechanism
//!
//! Line numbers below are at `839a242f3`, the kernel that still had the defect;
//! `0f44a11a7` moved them. Steps 1 through 4 are unchanged by the fix and still
//! describe what happens — a result still reaches a dead process's grant and
//! still comes back `InactiveApp`. Only step 5 changed: the `Err` is now
//! dropped the way `scan_done` always dropped it, instead of unwrapped.
//!
//! `capsules/extra/src/wifi/driver.rs:303` unwrapped `grants.enter`, which is
//! fallible. `scan_done` twenty lines above it, at `:280-286`, called the same
//! `grants.enter` and did not unwrap. That asymmetry was a kernel panic driven
//! by a radio interrupt, if the failing branch could be reached.
//!
//! Reading the kernel said it should be reachable, and the run above confirmed
//! every step. Each has a referent:
//!
//! 1. Nothing clears `WifiDriver::process_id` when a process dies — the capsule
//!    has no teardown hook — and `cyw4343`'s `self.pending` stays `Scan` until
//!    a scan-done event arrives (`cyw4343/driver.rs:470-476`).
//! 2. So the next `EscanResult` still calls `scanned_network` with the dead
//!    process's `ProcessId`.
//! 3. `Kernel::get_process` still finds a terminated process: it matches on
//!    array slot and identifier, and its own docs say a match "_will_ be found
//!    if the process still exists in the correct location in the array but is
//!    in any 'stopped' state" (`kernel/src/kernel.rs:102-124`).
//! 4. But `grant_is_allocated` returns `None` for anything not running
//!    (`process_standard.rs:1206-1210`), and `ProcessGrant::new_inner` turns
//!    that into `Err(Error::InactiveApp)` (`grant.rs:1152-1155`).
//! 5. `Grant::enter` propagates it (`grant.rs:1739-1743`), and `:303` unwrapped
//!    it. Since `0f44a11a7` it is `let _ =`, so the result is dropped.
//!
//! The one step reading could not establish was the first: whether a scan
//! result actually arrives in the window after the process is gone. Steps 3-5
//! are what the code does with such a result; whether one shows up is timing,
//! and timing is measured, not read. Sixty milliseconds into a scan of forty-odd
//! networks, one shows up.
//!
//! # Reading a rerun
//!
//! The process is gone before the verdict, so it lands on the console and at the
//! `tock$` prompt, not in this app's output.
//!
//! - **Kernel panic** — on a kernel carrying `0f44a11a7`, that is a regression,
//!   and this app is the repro for it. On one without, it is the original
//!   defect.
//! - **No panic, and the console still answers** — the fix is holding. Check
//!   `list` and `status` rather than the silence alone.
//! - **No panic and no console** — not a pass. A kernel that died quietly looks
//!   identical to one that survived, from the app's output.
//!
//! One thing this app still cannot distinguish, on any kernel: a run where no
//! result arrived after the process died looks exactly like a run where one
//! arrived and was handled. Two networks in 40 ms out of forty-odd says
//! delivery was live when it left, which is the best evidence available from
//! userspace, but it is not proof the window was entered. More networks in
//! range widens it.
//!
//! # Why it exits this early
//!
//! It leaves after two results rather than waiting for a round number. Two is
//! enough to prove the scan is live and delivering, and every result not yet
//! delivered is another chance for one to land after termination. A bench run
//! of `wifi_scan` saw 38 networks over 1500 ms, so leaving at two should leave
//! roughly three dozen still to come.
//!
//! Returning from `main` is a real termination: `Termination for ()` calls
//! `exit_terminate(0)` (`platform/src/termination.rs`). The scan is deliberately
//! **not** stopped first — stopping it is what this app must not do.

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
}

mod upcall {
    pub const INIT: u32 = 0;
    pub const SCAN_RES: u32 = 7;
}

mod rw_allow {
    pub const SCAN_SSID: u32 = 1;
}

/// Leave once this many networks have been reported. See the module docs.
const RESULTS_BEFORE_EXIT: u32 = 2;

const POLL_MS: u32 = 20;
const INIT_TIMEOUT_MS: u32 = 10_000;
/// Long enough that a scan which never delivers is reported rather than waited
/// on forever, short enough that the app is still inside the scan when it goes.
const RESULTS_TIMEOUT_MS: u32 = 10_000;

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

fn main() {
    let mut console = Console::writer();

    if TockSyscalls::command(DRIVER_NUM, command::EXISTS, 0, 0)
        .to_result::<(), ErrorCode>()
        .is_err()
    {
        let _ = writeln!(console, "wifi_scan_exit: no driver at {DRIVER_NUM:#x}");
        return;
    }

    if !init(&mut console) {
        return;
    }

    // Everything below happens inside the scan's share::scope, and the app
    // leaves from inside it. Dropping the scope unallows and unsubscribes on the
    // way out, which is fine: the window that matters opens after the process is
    // terminated, not while it is merely unsubscribed.
    let sink = ScanSink::default();
    let mut ssid = [0u8; 32];

    let started = share::scope::<
        (
            AllowRw<_, DRIVER_NUM, { rw_allow::SCAN_SSID }>,
            Subscribe<_, DRIVER_NUM, { upcall::SCAN_RES }>,
        ),
        _,
        _,
    >(|handle| {
        let (allow_rw, subscribe) = handle.split();

        TockSyscalls::allow_rw::<DefaultConfig, DRIVER_NUM, { rw_allow::SCAN_SSID }>(
            allow_rw, &mut ssid,
        )?;
        TockSyscalls::subscribe::<_, _, DefaultConfig, DRIVER_NUM, { upcall::SCAN_RES }>(
            subscribe, &sink,
        )?;

        TockSyscalls::command(DRIVER_NUM, command::SCAN, 0, 0).to_result::<(), ErrorCode>()?;

        Ok::<u32, ErrorCode>(wait_until(RESULTS_TIMEOUT_MS, || {
            sink.done.get() || sink.seen.get() >= RESULTS_BEFORE_EXIT
        }))
    });

    let waited = match started {
        Ok(waited) => waited,
        Err(e) => {
            let _ = writeln!(console, "wifi_scan_exit: scan could not start: {e:?}");
            return;
        }
    };

    let seen = sink.seen.get();

    if sink.done.get() {
        // The scan ended before this app could leave in the middle of it, so
        // there is no in-flight result to race and nothing was tested.
        let _ = writeln!(
            console,
            "wifi_scan_exit: INCONCLUSIVE — scan finished after {waited} ms with {seen} networks, \
             before the app could exit mid-scan"
        );
        return;
    }

    if seen == 0 {
        let _ = writeln!(
            console,
            "wifi_scan_exit: INCONCLUSIVE — no results within {RESULTS_TIMEOUT_MS} ms, \
             so the scan was not delivering and exiting proves nothing"
        );
        return;
    }

    // Say it before doing it: this is the last line the app gets to write, and
    // anything after it on the console came from the kernel.
    let _ = writeln!(
        console,
        "wifi_scan_exit: {seen} networks after {waited} ms — terminating now with the scan \
         still running. Anything below this line is the kernel's."
    );
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
            let _ = writeln!(console, "wifi_scan_exit: init could not start: {e:?}");
            return false;
        }
    };

    match status.get() {
        Some((0,)) => {
            let _ = writeln!(console, "wifi_scan_exit: init ok after {waited} ms");
            true
        }
        Some((status,)) => {
            let _ = writeln!(console, "wifi_scan_exit: init returned status {status}");
            false
        }
        None => {
            let _ = writeln!(
                console,
                "wifi_scan_exit: init did not call back within {INIT_TIMEOUT_MS} ms"
            );
            false
        }
    }
}
