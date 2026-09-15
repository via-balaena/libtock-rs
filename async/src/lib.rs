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

use core::future::{poll_fn, Future};
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

/// Runs two futures concurrently and returns both results.
///
/// Concurrency here is cooperative and single-threaded, as everything in a Tock
/// process is: each poll drives whichever future is ready, and the process parks
/// in `yield_wait` when neither is.
///
/// Both futures must be driving *different* drivers. Tock gives a process one
/// upcall slot per (driver, subscribe number), so joining two operations that
/// share a slot would have the second silently displace the first's
/// registration. Joining an alarm with a console read is fine; joining two
/// alarms is not.
///
/// Written with `poll_fn` over locals pinned in this function's own frame, which
/// is what keeps the crate free of `unsafe`: a hand-written `Join` future would
/// need pin projection to reach its fields.
pub async fn join<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut a_done: Option<A::Output> = None;
    let mut b_done: Option<B::Output> = None;

    poll_fn(move |context| {
        if a_done.is_none() {
            if let Poll::Ready(output) = a.as_mut().poll(context) {
                a_done = Some(output);
            }
        }
        if b_done.is_none() {
            if let Poll::Ready(output) = b.as_mut().poll(context) {
                b_done = Some(output);
            }
        }

        match (a_done.take(), b_done.take()) {
            (Some(first), Some(second)) => Poll::Ready((first, second)),
            (first, second) => {
                a_done = first;
                b_done = second;
                Poll::Pending
            }
        }
    })
    .await
}

/// Which side of a [`select`] finished first.
#[derive(Debug, Eq, PartialEq)]
pub enum Either<A, B> {
    Left(A),
    Right(B),
}

/// Runs two futures concurrently and returns the first result, cancelling the
/// other.
///
/// The loser is dropped when this function returns, which is what makes
/// cancellation load-bearing rather than theoretical: a dropped driver future
/// unallows its buffer, unsubscribes, and tells the driver to abort. `select` is
/// the reason all of that has to be correct.
///
/// The same one-slot-per-(driver, subscribe number) rule as [`join`] applies.
///
/// `a` is polled first, so if both are ready in the same pass, `Left` wins.
pub async fn select<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);

    poll_fn(move |context| {
        if let Poll::Ready(output) = a.as_mut().poll(context) {
            return Poll::Ready(Either::Left(output));
        }
        if let Poll::Ready(output) = b.as_mut().poll(context) {
            return Poll::Ready(Either::Right(output));
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests;
