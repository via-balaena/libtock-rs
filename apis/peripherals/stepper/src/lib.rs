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
