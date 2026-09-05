//! Hardware self-test for the async stack, reporting over the console.
//!
//! A blink proves almost nothing here, and the properties worth checking on
//! real silicon are the ones the fake kernel can only model:
//!
//! 1. An awaited `Sleep` resolves, after roughly the delay asked for.
//! 2. A `Sleep` dropped while armed does not poison the next one. That is the
//!    claim that unsubscribing clears the queued upcall (TRD 104) and that
//!    alarm command 3 disarms. Timing separates the failure modes: a stale
//!    upcall makes the next sleep finish instantly, while an unstopped alarm
//!    makes it finish late.

#![no_main]
#![no_std]

use core::fmt::Write;
use core::future::Future;
use core::pin::{pin, Pin};
use core::task::{Context, Poll};

use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::futures::block_on;
use libtock::runtime::{set_main, stack_size, TockSyscalls};

set_main! {main}
stack_size! {0x800}

/// Polls `inner` exactly once and reports whether it parked.
///
/// `true` means the operation registered with the kernel and is outstanding, so
/// dropping it afterwards exercises the cancellation path. `false` means it
/// resolved on the first poll, which for these probes means the driver refused
/// to start it.
struct ArmOnce<'a, F>(Pin<&'a mut F>);

impl<F: Future> Future for ArmOnce<'_, F> {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<bool> {
        Poll::Ready(self.0.as_mut().poll(context).is_pending())
    }
}

fn main() {
    let mut console = Console::writer();

    let freq = match Alarm::get_frequency() {
        Ok(freq) => freq.0,
        Err(_) => {
            let _ = writeln!(console, "async_probe: no alarm driver");
            return;
        }
    };
    let _ = writeln!(console, "async_probe: alarm at {} Hz", freq);

    block_on::<TockSyscalls, _>(async {
        // 1. A plain awaited sleep.
        let start = Alarm::get_ticks().unwrap_or(0);
        match Alarm::sleep_for_async(Milliseconds(500)) {
            Ok(sleep) => match sleep.await {
                Ok(()) => {
                    let elapsed = Alarm::get_ticks().unwrap_or(0).wrapping_sub(start);
                    let _ = writeln!(console, "await 500ms: {} ticks elapsed", elapsed);
                }
                Err(_) => {
                    let _ = writeln!(console, "await 500ms: FAILED");
                    return;
                }
            },
            Err(_) => {
                let _ = writeln!(console, "async_probe: sleep_for_async failed");
                return;
            }
        }

        // 2. Arm a five second sleep, then drop it. If cancellation works, the
        //    alarm is stopped and its upcall unregistered, so the next sleep
        //    behaves normally.
        if let Ok(long) = Alarm::sleep_for_async(Milliseconds(5000)) {
            let mut long = pin!(long);
            if !ArmOnce(long.as_mut()).await {
                let _ = writeln!(console, "5s sleep did not arm");
            }
        }
        let _ = writeln!(console, "cancelled an armed 5s sleep");

        let start = Alarm::get_ticks().unwrap_or(0);
        match Alarm::sleep_for_async(Milliseconds(500)) {
            Ok(sleep) => match sleep.await {
                Ok(()) => {
                    let elapsed = Alarm::get_ticks().unwrap_or(0).wrapping_sub(start);
                    let _ = writeln!(console, "await 500ms after cancel: {} ticks", elapsed);
                }
                Err(_) => {
                    let _ = writeln!(console, "await 500ms after cancel: FAILED");
                }
            },
            Err(_) => {
                let _ = writeln!(console, "sleep_for_async failed after cancel");
            }
        }

        // 3. Arm a console read and cancel it, twice. Dropping an armed `Read`
        //    aborts the receive (command 3), unallows the buffer and
        //    unsubscribes. If the abort did not land, the driver would still be
        //    mid-receive and the second read would be refused; if the buffer
        //    were not unallowed, the kernel would still hold a pointer into a
        //    dead future. Nothing is typed, so a read that arms stays parked.
        //    `read_async` needs no up-front syscall, so unlike
        //    `sleep_for_async` it hands back the future rather than a Result.
        for round in 1..=2 {
            let mut read = pin!(Console::read_async::<32>());
            let armed = ArmOnce(read.as_mut()).await;
            let _ = writeln!(
                console,
                "read {} armed: {}",
                round,
                if armed { "yes" } else { "REFUSED" }
            );
        }

        // Reaching here at all means the console survived two cancelled reads:
        // every line above went out over the same driver.
        let _ = writeln!(console, "console alive after two cancelled reads");

        let _ = writeln!(console, "async_probe: done");
    });
}
