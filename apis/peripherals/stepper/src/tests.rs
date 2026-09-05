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
            command::EXISTS | command::STOP => libtock_unittest::command_return::success(),
            command::STEP_FORWARD | command::STEP_REVERSE => {
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
