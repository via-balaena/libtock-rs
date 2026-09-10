//! Scans for WiFi networks through the `wifi` syscall driver, and reports what
//! the scan upcall actually delivered.
//!
//! # What this is for
//!
//! On the Pico 2 W the radio is reached over PIO gSPI, so a PIO interrupt has
//! to fire *and be serviced* for a scan to finish. A register read on a halted
//! board cannot separate "the interrupt never fired" from "it fired and was
//! cleared" — both leave the same bits behind. A completed scan can, because a
//! scan only terminates if upcalls keep arriving.
//!
//! So the verdict here is about the kernel's interrupt path, not about the
//! networks. The count and the terminator are the measurement; a recognisable
//! network name in the output is corroboration, not the point.
//!
//! # ABI
//!
//! Read out of the kernel tree at `839a242f3`,
//! `capsules/extra/src/wifi/driver.rs`, and `capsules/core/src/driver.rs:50`
//! for the number:
//!
//! ```text
//! driver   0x30008
//! command  0 exists, 1 init, 7 start scan, 8 stop scan
//! upcall   0 init done, 7 scan result *and* scan done
//! rw_allow 1 SSID of the current scan result
//! ```
//!
//! ## Upcall 7 means two things, and the length separates them
//!
//! `scanned_network` copies the SSID into rw_allow 1 and fires upcall 7 with
//! the SSID length in argument 0 (`driver.rs:301`); `scan_done` fires the same
//! upcall 7 with length 0 (`driver.rs:283`). Arguments 1 and 2 are 0 in both.
//! So the length is the only thing telling a result from the end of the scan.
//!
//! That is sound, and not by convention: `Ssid` carries its length as a
//! `NonZeroU8` (`wifi/device.rs:40`) and `Credential::try_new` rejects 0
//! (`device.rs:45-52`), so a result upcall cannot carry length 0. The cost is
//! paid at `cyw4343/driver.rs:467`, where `if let Ok(ssid) = Ssid::try_new(..)`
//! silently drops a network whose beacon carries a zero-length SSID — i.e. a
//! cloaked AP. Hidden networks are never reported, which is exactly why the
//! terminator is unambiguous.
//!
//! This app still treats an unexpected extra upcall after the terminator as
//! something to count and print rather than something that cannot happen.
//!
//! # What this app cannot see
//!
//! **It reads one SSID, not all of them.** The kernel writes every result over
//! the same rw_allow buffer at offset 0. libtock-rs hands that buffer to the
//! kernel for the lifetime of a `share::scope`, and the buffer is mutably
//! borrowed for exactly that long, so nothing can read it mid-scan without
//! first unallowing — which would drop the results that arrive in the gap.
//! The lengths of every result are recorded as they arrive; only the last
//! result's bytes survive to be printed.
//!
//! Delivering a stream of results through one overwritten buffer has no
//! lossless client. `libtock_ieee802154`'s receive path hit the same wall and
//! answered it with a kernel-side ring buffer.
//!
//! **The network count is a floor, not a total.** Upcalls are queued per
//! process, `schedule_upcall` fails when that queue is full, and
//! `driver.rs:300` discards the failure with `let _ =`. A dropped result is
//! silent, so a low count is not evidence that the air was quiet.
//!
//! The waiting below spends nearly all its time inside `yield_wait` — the sleep
//! keeps yielding until *its own* alarm fires, running any pending upcall each
//! time — so the queue is drained more or less continuously and a drop needs a
//! burst landing in the gap between sleeps. That is an argument, not a
//! measurement, and it applies to the terminator too: a dropped terminator and
//! an interrupt that never fired both print `NO TERMINATOR`. A run that reports
//! networks and then no terminator is the ambiguous case; zero networks and no
//! terminator is not.
//!
//! # The second pass, and why the length is worth distrusting
//!
//! `driver.rs:296-297` clamps the copy to the smaller of the SSID and the
//! caller's buffer, but `driver.rs:301` schedules the upcall with the
//! *unclamped* `ssid.len`. A caller whose buffer is shorter than the SSID is
//! told a length it did not receive, and reads stale bytes if it believes it.
//!
//! That is a code reading, so this app turns it into an observation. It scans
//! twice: once offering the kernel a 32-byte buffer, which is `wifi::len::SSID`
//! and therefore cannot truncate, and once offering 8 bytes, which truncates
//! most names. Both passes run back to back in one flash so the networks in
//! range are about the same for each, and the second pass counts how many
//! results reported more bytes than the buffer could hold. If that count is
//! nonzero while the first pass's count is zero, the mismatch is measured
//! rather than argued.
//!
//! # Measured
//!
//! All on a Pico 2 W against kernel `839a242f3`, run by the kernel session,
//! 2026-09-10.
//!
//! **First run, single 32-byte pass.** Init 500 ms, scan complete 1500 ms, 38
//! networks, terminator seen. That run is what closed the PIO interrupt
//! question: the terminator only arrives if the interrupt is being serviced.
//! The longest SSID it saw was 30 bytes, so nothing truncated.
//!
//! **Second run, both passes.** The clamp mismatch is now observed, not read:
//!
//! ```text
//! wifi_scan[full]:  43 networks, terminator seen, 32-byte buffer
//! wifi_scan[full]:  last SSID (16 bytes) "The2ndBiggestOof"
//! wifi_scan[short]: 37 networks, terminator seen, 8-byte buffer
//! wifi_scan[short]: 18 of 24 recorded results reported more than the 8 bytes
//! wifi_scan[short]: last SSID (13 bytes) "LaundryR"
//! ```
//!
//! `full` reported zero over-buffer results and `short` reported 18 of the 24
//! it recorded, which is the difference the two passes exist to produce. The
//! last line is the whole finding in one network: the kernel said 13 bytes and
//! wrote 8, so a caller that trusts the length reads 5 bytes it was never
//! given.
//!
//! Also settled, since this file previously recorded it as unverified: the
//! capsule **does** accept a second `command 7` after `scan_done`. Pass 2
//! started and completed normally.

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
    pub const SCAN_RES: u32 = 7;
}

mod rw_allow {
    pub const SCAN_SSID: u32 = 1;
}

/// The kernel's maximum SSID, `wifi::len::SSID`. Nothing can truncate against a
/// buffer this size.
const FULL_SSID_BUF: usize = 32;

/// Deliberately too small, to make the kernel clamp its copy while still
/// reporting the full length. The second pass uses it — see the module docs.
const SHORT_SSID_BUF: usize = 8;

/// How many result lengths to keep. A scan that finds more than this still
/// counts them all; it just stops recording their lengths.
const CAP: usize = 24;

/// Waits are polled rather than blocked on, so a driver that never calls back
/// prints a verdict instead of hanging with nobody able to tell why.
const POLL_MS: u32 = 100;
const INIT_TIMEOUT_MS: u32 = 10_000;
const SCAN_TIMEOUT_MS: u32 = 20_000;

/// Accumulates every scan upcall.
///
/// A `Cell<Option<(u32,)>>` would keep only the newest, and several upcalls can
/// be delivered before this app looks again — each `yield` runs one pending
/// callback, and the bounded wait below yields inside `Alarm::sleep_for`. So
/// this records as it goes.
#[derive(Default)]
struct ScanSink {
    /// Reported lengths in arrival order, up to `CAP`.
    lens: [Cell<u8>; CAP],
    /// The most recent reported length, which is the one describing whatever is
    /// left in the shared buffer. Kept separately from `lens` because past
    /// `CAP` that array stops growing while the buffer keeps being overwritten.
    last_len: Cell<u32>,
    /// Networks reported, counting any past `CAP`.
    seen: Cell<u32>,
    /// The zero-length upcall that ends a scan has arrived.
    done: Cell<bool>,
    /// Upcalls after the terminator. Expected to stay 0.
    after_done: Cell<u32>,
}

impl Upcall<AnyId> for ScanSink {
    fn upcall(&self, len: u32, _: u32, _: u32) {
        if self.done.get() {
            self.after_done.set(self.after_done.get() + 1);
            return;
        }
        if len == 0 {
            self.done.set(true);
            return;
        }
        let n = self.seen.get();
        if (n as usize) < CAP {
            self.lens[n as usize].set(len.min(u8::MAX as u32) as u8);
        }
        self.last_len.set(len);
        self.seen.set(n + 1);
    }
}

/// Polls `ready` until it holds, or until `timeout_ms` has elapsed. Returns the
/// milliseconds actually waited, so a caller can print how long it took rather
/// than just that it finished.
///
/// Sleeping is what yields here, and the driver's upcalls are still subscribed
/// while it sleeps, so they are delivered during these waits.
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
        let _ = writeln!(
            console,
            "wifi_scan: no driver at {DRIVER_NUM:#x} — this kernel has no wifi capsule"
        );
        return;
    }
    let _ = writeln!(console, "wifi_scan: driver {DRIVER_NUM:#x} present");

    if !init(&mut console) {
        return;
    }

    // Two passes over the same air, differing only in the buffer offered to the
    // kernel. The first can never truncate; the second is too small for most
    // names. Running them back to back in one flash keeps the networks in range
    // roughly constant, so a difference between the two lines is the buffer
    // size and not the neighbourhood.
    scan(&mut console, &mut [0u8; FULL_SSID_BUF], "full");
    scan(&mut console, &mut [0u8; SHORT_SSID_BUF], "short");
}

/// Command 1, then upcall 0. Argument 0 of that upcall is a statuscode.
///
/// Returns whether the radio came up. The reason is printed either way, so the
/// caller has nothing to add.
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
            let _ = writeln!(console, "wifi_scan: init could not start: {e:?}");
            return false;
        }
    };

    match status.get() {
        None => {
            let _ = writeln!(
                console,
                "wifi_scan: init did not call back within {INIT_TIMEOUT_MS} ms"
            );
            false
        }
        Some((0,)) => {
            let _ = writeln!(console, "wifi_scan: init ok after {waited} ms");
            true
        }
        Some((status,)) => {
            let _ = writeln!(console, "wifi_scan: init returned status {status}");
            false
        }
    }
}

/// Command 7, then upcall 7 per network, then upcall 7 with length 0.
///
/// `ssid` is the buffer offered to the kernel as rw_allow 1, and its length is
/// the whole variable under test between the two passes. `label` names the pass
/// in the output.
fn scan(console: &mut impl Write, ssid: &mut [u8], label: &str) {
    let sink = ScanSink::default();
    let mut waited = 0;

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
            allow_rw, ssid,
        )?;
        TockSyscalls::subscribe::<_, _, DefaultConfig, DRIVER_NUM, { upcall::SCAN_RES }>(
            subscribe, &sink,
        )?;

        TockSyscalls::command(DRIVER_NUM, command::SCAN, 0, 0).to_result::<(), ErrorCode>()?;

        waited = wait_until(SCAN_TIMEOUT_MS, || sink.done.get());
        Ok::<(), ErrorCode>(())
    });

    let buf_len = ssid.len();

    if let Err(e) = started {
        let _ = writeln!(console, "wifi_scan[{label}]: scan could not start: {e:?}");
        return;
    }

    let seen = sink.seen.get();

    if sink.done.get() {
        let _ = writeln!(
            console,
            "wifi_scan[{label}]: scan complete after {waited} ms — {seen} networks, \
             terminator seen, {buf_len}-byte buffer"
        );
    } else {
        // No terminator: the radio never finished. Try to leave it stopped
        // rather than scanning into whatever runs next.
        //
        // Expect this to report Err(BUSY) and change nothing. `init_tasks`
        // refuses every device operation while a scan is outstanding
        // (`cyw4343/driver.rs:236-237`) and `stop_scan` propagates that before
        // it queues anything (`:670-678`), so command 8 cannot stop a scan —
        // only a scan that has already ended will accept it. The result is
        // printed rather than discarded precisely because it is expected to
        // fail: do not "fix" this by dropping it.
        let stop = TockSyscalls::command(DRIVER_NUM, command::STOP_SCAN, 0, 0)
            .to_result::<(), ErrorCode>();
        let _ = writeln!(
            console,
            "wifi_scan[{label}]: NO TERMINATOR within {SCAN_TIMEOUT_MS} ms — {seen} networks, \
             stop_scan {stop:?}"
        );
    }

    if sink.after_done.get() != 0 {
        let _ = writeln!(
            console,
            "wifi_scan[{label}]: {} upcalls arrived after the terminator",
            sink.after_done.get()
        );
    }

    if seen == 0 {
        return;
    }

    let recorded = (seen as usize).min(CAP);
    let _ = write!(console, "wifi_scan[{label}]: lengths");
    for len in &sink.lens[..recorded] {
        let _ = write!(console, " {}", len.get());
    }
    if seen as usize > recorded {
        let _ = write!(console, " (+{} not recorded)", seen as usize - recorded);
    }
    let _ = writeln!(console);

    // The finding, stated as a count rather than an argument: every one of these
    // is a result the kernel described as longer than the buffer it copied into.
    let over = sink.lens[..recorded]
        .iter()
        .filter(|len| len.get() as usize > buf_len)
        .count();
    if over != 0 {
        let _ = writeln!(
            console,
            "wifi_scan[{label}]: {over} of {recorded} recorded results reported more than the \
             {buf_len} bytes the buffer could hold (driver.rs:296 clamps the copy, :301 does not \
             clamp the length)"
        );
    }

    // Only the last result's bytes are still in the buffer; everything before it
    // was overwritten. Print exactly its reported length, clamped to what the
    // buffer could actually hold, and say so when those two differ.
    let reported = sink.last_len.get() as usize;
    let readable = reported.min(buf_len);
    let _ = write!(
        console,
        "wifi_scan[{label}]: last SSID ({reported} bytes) \""
    );
    for &b in &ssid[..readable] {
        let _ = write!(
            console,
            "{}",
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        );
    }
    let _ = writeln!(console, "\"");
    if reported > buf_len {
        let _ = writeln!(
            console,
            "wifi_scan[{label}]: reported length {reported} exceeds the {buf_len}-byte buffer, \
             so {} of those bytes were never written and only {readable} are shown above",
            reported - readable
        );
    }
}
