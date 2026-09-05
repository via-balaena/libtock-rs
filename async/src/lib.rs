//! Driving `Future`s inside a Tock process.
//!
//! Tock delivers upcalls to a process only while it is inside a Yield system
//! call. That single fact makes an executor here much simpler than a bare-metal
//! one: there is no interrupt context, so nothing an upcall touches needs to be
//! atomic, and a future can never be woken between a poll and the yield that
//! follows it.
//!
//! [`block_on`] is the cheapest tier of that idea — it drives one future and
//! needs no task storage. A multi-task executor would sit at this same layer,
//! over the same driver futures.

#![cfg_attr(not(test), no_std)]
// The executor layer needs no `unsafe` of its own: everything subtle lives in
// `libtock_platform::async_call`, audited once. Lock that in.
#![forbid(unsafe_code)]

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use libtock_platform::Syscalls;

/// Runs `future` to completion, sleeping in `yield_wait` whenever it is pending.
///
/// A no-op waker is enough here. Waking exists to tell an executor that a task
/// became ready while it was not looking, but this loop re-polls
/// unconditionally after every yield, and an upcall can only arrive inside that
/// yield. Futures are still expected to store the waker they are given, so the
/// same driver futures work unchanged under an executor that does need it.
///
/// ```ignore
/// # use libtock_alarm::{Alarm, Milliseconds};
/// # type Syscalls = libtock_runtime::TockSyscalls;
/// libtock_async::block_on::<Syscalls, _>(async {
///     Alarm::<Syscalls>::sleep_for_async(Milliseconds(500))?.await
/// })?;
/// ```
pub fn block_on<S: Syscalls, F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());

    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        S::yield_wait();
    }
}

#[cfg(test)]
mod tests;
