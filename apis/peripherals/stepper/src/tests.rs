use core::cell::Cell;

use libtock_platform::{CommandReturn, ErrorCode};
use libtock_unittest::{fake, DriverInfo, DriverShareRef};

use crate::{command, subscribe, Interval, DRIVER_NUM};

type Stepper = crate::Stepper<fake::Syscalls>;

/// A stepper capsule that completes instantly, recording what it was asked for.
///
/// Defined here rather than in `libtock_unittest` because there is no upstream
/// capsule to model yet — the kernel side is still being written, so a shared
/// fake would be a fake of something that does not exist.
#[derive(Default)]
struct FakeStepper {
    share_ref: DriverShareRef,
    last_command: Cell<Option<(u32, u32, u32)>>,
    /// Status reported through the completion upcall.
    status: Cell<u32>,
    /// Steps reported as taken. `None` means "however many were asked for".
    taken: Cell<Option<u32>>,
    /// When set, a movement stays outstanding until something stops it, the way
    /// a real one does. The default completes instantly, which the blocking
    /// tests depend on.
    deferred: Cell<bool>,
    /// The movement currently running, if the fake is deferred.
    running: Cell<bool>,
}

impl FakeStepper {
    fn new() -> std::rc::Rc<FakeStepper> {
        std::rc::Rc::new(Default::default())
    }

    /// Makes the next movement report a failure.
    fn fail_with(&self, status: ErrorCode) {
        self.status.set(status as u32);
    }

    /// Makes the next movement report fewer steps than requested, as a stopped
    /// movement would.
    fn report_taken(&self, steps: u32) {
        self.taken.set(Some(steps));
    }

    /// A fake whose movements stay outstanding until stopped.
    fn new_deferred() -> std::rc::Rc<FakeStepper> {
        let driver: std::rc::Rc<FakeStepper> = Default::default();
        driver.deferred.set(true);
        driver
    }

    /// Whether a movement is outstanding. The point of the deferred mode: it
    /// makes "the coils are still energised" observable to a test.
    fn is_running(&self) -> bool {
        self.running.get()
    }
}

impl fake::SyscallDriver for FakeStepper {
    fn info(&self) -> DriverInfo {
        DriverInfo::new(DRIVER_NUM).upcall_count(1)
    }

    fn register(&self, share_ref: DriverShareRef) {
        self.share_ref.replace(share_ref);
    }

    fn command(&self, command_id: u32, argument0: u32, argument1: u32) -> CommandReturn {
        self.last_command
            .set(Some((command_id, argument0, argument1)));

        match command_id {
            command::EXISTS => libtock_unittest::command_return::success(),
            command::STOP => {
                // A stop still reports. That is what lets a caller who stops a
                // movement learn where the motor ended up, and it is the
                // behaviour the async cancellation path depends on.
                if self.running.replace(false) {
                    self.share_ref
                        .schedule_upcall(
                            subscribe::COMPLETE,
                            (self.status.get(), self.taken.get().unwrap_or(0), 0),
                        )
                        .expect("schedule_upcall failed");
                }
                libtock_unittest::command_return::success()
            }
            command::STEP_FORWARD | command::STEP_REVERSE => {
                if self.deferred.get() {
                    self.running.set(true);
                    return libtock_unittest::command_return::success();
                }
                let taken = self.taken.get().unwrap_or(argument0);
                self.share_ref
                    .schedule_upcall(subscribe::COMPLETE, (self.status.get(), taken, 0))
                    .expect("schedule_upcall failed");
                libtock_unittest::command_return::success()
            }
            _ => libtock_unittest::command_return::failure(ErrorCode::NoSupport),
        }
    }
}

fn kernel_with_stepper() -> (fake::Kernel, std::rc::Rc<FakeStepper>) {
    let kernel = fake::Kernel::new();
    let driver = FakeStepper::new();
    kernel.add_driver(&driver);
    (kernel, driver)
}

#[test]
fn exists() {
    let (_kernel, _driver) = kernel_with_stepper();
    assert_eq!(Stepper::exists(), Ok(()));
}

#[test]
fn no_driver() {
    let _kernel = fake::Kernel::new();
    assert_eq!(Stepper::exists(), Err(ErrorCode::NoDevice));
}

#[test]
fn step_forward_reports_steps_taken() {
    let (_kernel, driver) = kernel_with_stepper();

    assert_eq!(Stepper::step_forward(4096, Interval(2000)), Ok(4096));
    assert_eq!(
        driver.last_command.get(),
        Some((command::STEP_FORWARD, 4096, 2000)),
        "the count and interval must reach the capsule unchanged"
    );
}

#[test]
fn step_reverse_uses_its_own_command() {
    let (_kernel, driver) = kernel_with_stepper();

    assert_eq!(Stepper::step_reverse(512, Interval(1500)), Ok(512));
    assert_eq!(
        driver.last_command.get(),
        Some((command::STEP_REVERSE, 512, 1500)),
        "direction is a command, not a flag in an argument"
    );
}

/// A stopped movement completes with fewer steps than asked for, which is a
/// success rather than an error — the caller needs the count to know where the
/// motor ended up.
#[test]
fn a_short_movement_is_not_an_error() {
    let (_kernel, driver) = kernel_with_stepper();
    driver.report_taken(1200);

    assert_eq!(Stepper::step_forward(4096, Interval(2000)), Ok(1200));
}

#[test]
fn failure_status_propagates() {
    let (_kernel, driver) = kernel_with_stepper();
    driver.fail_with(ErrorCode::Reserve);

    assert_eq!(
        Stepper::step_forward(100, Interval(2000)),
        Err(ErrorCode::Reserve),
        "a capsule that refuses because another process owns the motor"
    );
}

#[test]
fn stop_is_a_bare_command() {
    let (_kernel, driver) = kernel_with_stepper();

    assert_eq!(Stepper::stop(), Ok(()));
    assert_eq!(driver.last_command.get(), Some((command::STOP, 0, 0)));
}

// -----------------------------------------------------------------------------
// Async interface
// -----------------------------------------------------------------------------

/// Tests for [`Step`](crate::Step).
///
/// The subject here is the step count on the cancellation path. For an
/// open-loop motor the count is the position, so "how does this future end" and
/// "does the caller still know where the shaft is" are the same question.
#[cfg(feature = "async")]
mod step {
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use libtock_platform::{Syscalls, YieldNoWaitReturn};

    use super::*;

    fn kernel_with_deferred_stepper() -> (fake::Kernel, std::rc::Rc<FakeStepper>) {
        let kernel = fake::Kernel::new();
        let driver = FakeStepper::new_deferred();
        kernel.add_driver(&driver);
        (kernel, driver)
    }

    #[test]
    fn step_resolves_with_the_count() {
        let (_kernel, driver) = kernel_with_stepper();
        driver.report_taken(1200);

        let mut step = pin!(Stepper::step_forward_async(4096, Interval(2000)));
        let mut context = Context::from_waker(Waker::noop());

        assert_eq!(step.as_mut().poll(&mut context), Poll::Pending);
        fake::Syscalls::yield_wait();
        assert_eq!(step.as_mut().poll(&mut context), Poll::Ready(Ok(1200)));
    }

    /// Nothing reaches the capsule until the future is polled, so a `Step` that
    /// is built and dropped must not have moved the motor.
    #[test]
    fn an_unpolled_step_never_starts() {
        let (_kernel, driver) = kernel_with_deferred_stepper();

        drop(Stepper::step_forward_async(4096, Interval(2000)));

        assert_eq!(driver.last_command.get(), None);
        assert!(!driver.is_running());
    }

    /// The cancellation path does the safety-critical half right: the motor
    /// stops and the coils de-energise. It also throws the count away, which is
    /// the cost this documents rather than hides.
    #[test]
    fn dropping_a_step_stops_the_motor_and_loses_the_count() {
        let (_kernel, driver) = kernel_with_deferred_stepper();
        driver.report_taken(1200);

        {
            let mut step = pin!(Stepper::step_forward_async(4096, Interval(2000)));
            let mut context = Context::from_waker(Waker::noop());
            assert_eq!(step.as_mut().poll(&mut context), Poll::Pending);
            assert!(
                driver.is_running(),
                "polling should have started a movement"
            );
        }

        assert!(
            !driver.is_running(),
            "dropping a Step must stop the motor, not merely unsubscribe"
        );
        assert_eq!(driver.last_command.get(), Some((command::STOP, 0, 0)));

        // The count the stop reported went nowhere: the upcall it scheduled is
        // cleared by the unsubscribe that follows a cancellation. Nothing in
        // the process now knows the motor turned 1200 steps.
        assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
    }

    /// The way to stop a movement without losing the position: stop the motor
    /// while the future is still alive, and let it resolve with the partial
    /// count. This is what the `select(step.as_mut(), ..)` pattern in [`Step`]
    /// buys — the borrow loses the select, the movement does not.
    #[test]
    fn stopping_a_live_step_resolves_it_with_the_partial_count() {
        let (_kernel, driver) = kernel_with_deferred_stepper();
        driver.report_taken(1200);

        let mut step = pin!(Stepper::step_forward_async(4096, Interval(2000)));
        let mut context = Context::from_waker(Waker::noop());

        assert_eq!(step.as_mut().poll(&mut context), Poll::Pending);
        assert!(driver.is_running());

        assert_eq!(Stepper::stop(), Ok(()));
        fake::Syscalls::yield_wait();

        assert_eq!(
            step.as_mut().poll(&mut context),
            Poll::Ready(Ok(1200)),
            "a stopped movement still reports where it got to"
        );
        assert!(!driver.is_running());
    }
}
