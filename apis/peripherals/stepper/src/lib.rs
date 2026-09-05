#![cfg_attr(not(test), no_std)]

use core::cell::Cell;
use libtock_platform as platform;
use libtock_platform::share;
use libtock_platform::{DefaultConfig, ErrorCode, Syscalls};

/// The stepper motor driver.
///
/// The kernel owns the stepping loop: a `step_forward` or `step_reverse` runs
/// the phase sequence from the capsule's own alarm, so the syscall round trip is
/// paid once per movement rather than once per step, and the step rate is set by
/// the motor rather than by syscall overhead.
///
/// That placement is also what makes the coils safe. A capsule stepping on its
/// own alarm can check on each tick whether the process that asked for the
/// movement is still alive, and de-energise if it is not. An application driving
/// the same motor through raw GPIO cannot: nothing releases a GPIO pin when a
/// process dies, so a fault mid-step leaves a coil energised for as long as the
/// board is powered.
///
/// # Example
/// ```ignore
/// use libtock::stepper::{Interval, Stepper};
///
/// // One revolution of a 28BYJ-48 in half-step mode, 2 ms per step.
/// Stepper::step_forward(4096, Interval(2000))?;
/// ```
pub struct Stepper<S: Syscalls, C: platform::subscribe::Config = DefaultConfig>(S, C);

/// Microseconds between steps.
///
/// Microseconds rather than milliseconds because the capsule owns the timing, so
/// the resolution costs nothing and a faster motor will not need an ABI change.
/// A `u32` still spans about seventy minutes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Interval(pub u32);

impl<S: Syscalls, C: platform::subscribe::Config> Stepper<S, C> {
    /// Checks that the stepper capsule is present.
    #[inline(always)]
    pub fn exists() -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, command::EXISTS, 0, 0).to_result()
    }

    /// Steps forward, blocking until the movement finishes. Returns the number
    /// of steps actually taken, which is fewer than requested if the movement
    /// was stopped.
    ///
    /// A step is one phase advance. The capsule half-steps, so a 28BYJ-48 takes
    /// 4096 of these per output revolution rather than 2048 — full steps are two
    /// of them. The finer unit is the primitive so that callers who want it are
    /// not locked out.
    ///
    /// Returns `Busy` if a movement is already running, including one this
    /// process started. Call [`Stepper::stop`] first to redirect: it reports the
    /// partial count, so nothing is lost by stopping before changing course.
    pub fn step_forward(steps: u32, interval: Interval) -> Result<u32, ErrorCode> {
        Self::run(command::STEP_FORWARD, steps, interval)
    }

    /// Steps in reverse. See [`Stepper::step_forward`].
    pub fn step_reverse(steps: u32, interval: Interval) -> Result<u32, ErrorCode> {
        Self::run(command::STEP_REVERSE, steps, interval)
    }

    /// Stops a movement and de-energises the coils.
    ///
    /// The stopped movement still reports: its completion upcall carries the
    /// partial step count, which for an open-loop motor is the only record of
    /// where it ended up. So a `step_forward` blocked in its yield loop returns
    /// `Ok(partial)` rather than hanging or losing the position.
    ///
    /// Returns `Reserve` if another process owns the motor. It does not
    /// silently do nothing: a supervisor trying to stop a hung owner needs to
    /// learn that it did not, and a silent no-op is the wrong failure mode for
    /// the one command whose purpose is making something stop.
    ///
    /// That also means the owner cannot reach this from a blocking call — it is
    /// inside `step_forward` for the whole movement. Reachable from an upcall
    /// handler, or from a future's cancellation path once there is an async
    /// driver.
    pub fn stop() -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, command::STOP, 0, 0).to_result()
    }

    fn run(command_num: u32, steps: u32, interval: Interval) -> Result<u32, ErrorCode> {
        let called: Cell<Option<(u32, u32)>> = Cell::new(None);

        share::scope(|subscribe| {
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::COMPLETE }>(subscribe, &called)?;

            S::command(DRIVER_NUM, command_num, steps, interval.0).to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((status, taken)) = called.get() {
                    return match status {
                        0 => Ok(taken),
                        other => Err(other.try_into().unwrap_or(ErrorCode::Fail)),
                    };
                }
            }
        })
    }
}

// -----------------------------------------------------------------------------
// Async interface
// -----------------------------------------------------------------------------

/// The stepper-specific half of a [`Step`].
#[cfg(feature = "async")]
pub struct StepOp {
    command_num: u32,
    steps: u32,
    interval: Interval,
}

#[cfg(feature = "async")]
impl<S: Syscalls> platform::async_call::Operation<S> for StepOp {
    /// Steps actually taken, fewer than requested if the movement was stopped.
    type Value = u32;

    fn start(&self) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, self.command_num, self.steps, self.interval.0).to_result()
    }

    fn cancel(&self) {
        // This is the path [`Stepper::stop`] could not previously be reached
        // from: a blocking `step_forward` holds the caller for the whole
        // movement, so only an upcall handler could stop it. Dropping a `Step`
        // now stops the motor from the owning process.
        //
        // It also throws away the step count. The stop provokes a completion
        // upcall carrying the partial count, and the unsubscribe that follows a
        // cancellation clears it (TRD 104) -- so a dropped `Step` de-energises
        // the coils correctly and forgets where the motor is, which for an
        // open-loop motor is the whole position. [`Step`] documents what to do
        // instead.
        let _ = S::command(DRIVER_NUM, command::STOP, 0, 0);
    }

    fn complete(&self, args: (u32, u32, u32)) -> Result<u32, ErrorCode> {
        match args.0 {
            0 => Ok(args.1),
            other => Err(other.try_into().unwrap_or(ErrorCode::Fail)),
        }
    }
}

/// A future that resolves when a movement finishes, with the number of steps
/// taken. Create one with [`Stepper::step_forward_async`].
///
/// # Stopping without losing the position
///
/// Dropping a `Step` stops the motor but discards the partial step count, and
/// for an open-loop motor that count is the only record of where the shaft
/// ended up. `select` drops its loser, so the obvious spelling loses it:
///
/// ```ignore
/// // Stops the motor, and forgets how far it got.
/// select(Stepper::step_forward_async(4096, Interval(2000)), press).await
/// ```
///
/// Lend the future to `select` instead of giving it away. `Pin<&mut F>` is
/// itself a `Future`, so the loser that `select` drops is the borrow, not the
/// movement -- the subscription survives, and the completion upcall that the
/// stop provokes still has somewhere to land:
///
/// ```ignore
/// let mut step = pin!(Stepper::step_forward_async(4096, Interval(2000)));
/// let press = pin!(button.next_edge(PinInterruptEdge::Falling));
///
/// let taken = match select(step.as_mut(), press).await {
///     Either::Left(done) => done?,      // the movement finished on its own
///     Either::Right(_) => {
///         Stepper::stop()?;             // ask the capsule to stop ...
///         step.await?                   // ... and collect the partial count
///     }
/// };
/// ```
#[cfg(feature = "async")]
pub type Step<S, C = DefaultConfig> =
    platform::async_call::Call<S, C, StepOp, DRIVER_NUM, { subscribe::COMPLETE }>;

#[cfg(feature = "async")]
impl<S: Syscalls, C: platform::subscribe::Config> Stepper<S, C> {
    /// Returns a future that steps forward and resolves with the steps taken.
    ///
    /// Nothing is issued until the future is first polled, so a `Step` that is
    /// built and dropped never moves the motor. See [`Step`] for how to stop one
    /// without losing the count.
    pub fn step_forward_async(steps: u32, interval: Interval) -> Step<S, C> {
        platform::async_call::Call::new(StepOp {
            command_num: command::STEP_FORWARD,
            steps,
            interval,
        })
    }

    /// Steps in reverse. See [`Stepper::step_forward_async`].
    pub fn step_reverse_async(steps: u32, interval: Interval) -> Step<S, C> {
        platform::async_call::Call::new(StepOp {
            command_num: command::STEP_REVERSE,
            steps,
            interval,
        })
    }
}

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------------
// Driver number, command IDs and subscribe IDs
// -----------------------------------------------------------------------------

// Provisional: 0x11 is free below 0x20 and sits directly after PWM at 0x10 in
// the Hardware Access block. A number becomes real by being added to the table
// in tock's doc/syscalls/README.md, so this may move.
const DRIVER_NUM: u32 = 0x00011;

#[allow(unused)]
mod command {
    pub const EXISTS: u32 = 0;
    pub const STEP_FORWARD: u32 = 1;
    pub const STEP_REVERSE: u32 = 2;
    pub const STOP: u32 = 3;
}

#[allow(unused)]
mod subscribe {
    pub const COMPLETE: u32 = 0;
}
