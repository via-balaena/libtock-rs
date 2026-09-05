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
use libtock::futures::{block_on, select, Either};
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

        // 4. select with a timeout that wins. The losing read is dropped while
        //    genuinely outstanding, with the alarm still in flight alongside it
        //    -- the first time two operations are live at once on hardware.
        let start = Alarm::get_ticks().unwrap_or(0);
        match select(
            Alarm::sleep_for_async(Milliseconds(500)).expect("no alarm driver"),
            Console::read_async::<16>(),
        )
        .await
        {
            Either::Left(_) => {
                let elapsed = Alarm::get_ticks().unwrap_or(0).wrapping_sub(start);
                let _ = writeln!(console, "select: timeout won after {} ticks", elapsed);
            }
            Either::Right(_) => {
                let _ = writeln!(console, "select: read won (was something typed?)");
            }
        }

        // This line going out at all proves the cancelled read left the console
        // usable. The sleep that follows proves it left the alarm usable too: a
        // teardown that disturbed either would show up as a wrong number.
        let start = Alarm::get_ticks().unwrap_or(0);
        if Alarm::sleep_for_async(Milliseconds(500))
            .expect("no alarm driver")
            .await
            .is_ok()
        {
            let elapsed = Alarm::get_ticks().unwrap_or(0).wrapping_sub(start);
            let _ = writeln!(console, "after select: 500ms -> {} ticks", elapsed);
        }

        // 5. The other direction: a read that wins, cancelling a five second
        //    alarm. Needs a byte within five seconds, so this reports which way
        //    it went rather than assuming.
        let _ = writeln!(console, "select: send a byte within 5s");
        match select(
            Alarm::sleep_for_async(Milliseconds(5000)).expect("no alarm driver"),
            Console::read_async::<16>(),
        )
        .await
        {
            Either::Right(Ok(output)) => {
                let _ = writeln!(
                    console,
                    "select: read won with {} bytes, alarm cancelled",
                    output.count()
                );
            }
            Either::Right(Err(_)) => {
                let _ = writeln!(console, "select: read won but failed");
            }
            Either::Left(_) => {
                let _ = writeln!(console, "select: timed out, no byte arrived");
            }
        }

        // If the losing five second alarm was not stopped, this sleep is the
        // place it would show: a stale deadline or a leaked upcall both land
        // far from 500k ticks.
        let start = Alarm::get_ticks().unwrap_or(0);
        if Alarm::sleep_for_async(Milliseconds(500))
            .expect("no alarm driver")
            .await
            .is_ok()
        {
            let elapsed = Alarm::get_ticks().unwrap_or(0).wrapping_sub(start);
            let _ = writeln!(console, "after 5s cancel: 500ms -> {} ticks", elapsed);
        }

        let _ = writeln!(console, "async_probe: done");
    });
}
