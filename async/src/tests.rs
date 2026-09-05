use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use libtock_alarm::{Alarm, Milliseconds, Ticks};
use libtock_platform::ErrorCode;
use libtock_unittest::{fake, SyscallLogEntry};

type TestAlarm = Alarm<fake::Syscalls>;

const DRIVER_NUM: u32 = 0;
const CALLBACK: u32 = 0;
const STOP: u32 = 3;
const SET_RELATIVE: u32 = 5;

/// The kernel and driver both have to outlive the test body, so hand both back.
fn alarm_kernel() -> (fake::Kernel, std::rc::Rc<fake::Alarm>) {
    let kernel = fake::Kernel::new();
    let driver = fake::Alarm::new(1000);
    kernel.add_driver(&driver);
    (kernel, driver)
}

fn subscribe_count(log: &[SyscallLogEntry]) -> usize {
    log.iter()
        .filter(|entry| {
            matches!(
                entry,
                SyscallLogEntry::Subscribe {
                    driver_num: DRIVER_NUM,
                    subscribe_num: CALLBACK,
                }
            )
        })
        .count()
}

#[test]
fn sleep_resolves() {
    let (_kernel, _driver) = alarm_kernel();

    let result = crate::block_on::<fake::Syscalls, _>(
        TestAlarm::sleep_for_async(Ticks(1000)).expect("frequency lookup failed"),
    );

    assert_eq!(result, Ok(()));
}

/// Proves the future composes with `.await` and that the upcall slot is
/// reusable: the first `Sleep` must have unsubscribed on drop for the second to
/// register successfully.
#[test]
fn sequential_sleeps_share_one_slot() {
    let (_kernel, _driver) = alarm_kernel();

    async fn two_sleeps() -> Result<(), ErrorCode> {
        TestAlarm::sleep_for_async(Milliseconds(10))?.await?;
        TestAlarm::sleep_for_async(Milliseconds(10))?.await
    }

    assert_eq!(crate::block_on::<fake::Syscalls, _>(two_sleeps()), Ok(()));
}

/// The cancellation case: a future dropped after it has registered its upcall
/// but before it fires must unsubscribe, or the kernel keeps a pointer to
/// memory the process is about to reuse. This is the property that makes the
/// future safe to drop inside a `select`.
#[test]
fn drop_before_firing_unsubscribes() {
    let (kernel, _driver) = alarm_kernel();
    let _ = kernel.take_syscall_log();

    {
        let sleep = TestAlarm::sleep_for_async(Milliseconds(10)).expect("frequency lookup failed");
        let mut sleep = pin!(sleep);
        let mut context = Context::from_waker(Waker::noop());

        // One poll registers the upcall and arms the alarm. No yield happens
        // here, and Tock only delivers upcalls inside a yield, so the future
        // cannot have completed.
        assert_eq!(sleep.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(
            subscribe_count(&kernel.take_syscall_log()),
            1,
            "polling should have registered exactly one upcall"
        );
    }

    let log = kernel.take_syscall_log();

    assert_eq!(
        subscribe_count(&log),
        1,
        "dropping an armed Sleep must unsubscribe"
    );
    // Reach of this assertion: `fake::Alarm` implements only FREQUENCY and
    // SET_RELATIVE, so it answers the stop with NoSupport. This proves the
    // syscall is issued, not that a pending alarm is disarmed. The disarming is
    // Tock's `capsules/core/src/alarm.rs` command 3, which clears the stored
    // expiration and ignores both arguments.
    assert!(
        log.iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::Command {
                driver_id: DRIVER_NUM,
                command_id: STOP,
                ..
            }
        )),
        "dropping an armed Sleep must also stop the alarm, or the capsule keeps \
         a virtual alarm set for a callback that no longer exists"
    );
}

/// A `Sleep` that is never polled must never arm the alarm.
///
/// It does still issue one unsubscribe on drop: `Subscribe::drop` runs
/// unconditionally, and the kernel treats unsubscribing an unregistered slot as
/// a no-op. Keeping that drop unconditional is what makes the cancellation path
/// above trivially correct, and the cost is a single syscall on a rare path.
#[test]
fn drop_before_polling_does_not_arm_alarm() {
    let (kernel, _driver) = alarm_kernel();
    let _ = kernel.take_syscall_log();

    drop(TestAlarm::sleep_for_async(Milliseconds(10)).expect("frequency lookup failed"));

    let log = kernel.take_syscall_log();

    assert!(
        !log.iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::Command {
                driver_id: DRIVER_NUM,
                command_id: SET_RELATIVE,
                ..
            }
        )),
        "an unpolled Sleep must never arm the alarm"
    );
    assert_eq!(
        subscribe_count(&log),
        1,
        "drop issues one no-op unsubscribe even though nothing was registered"
    );
}

/// A `Sleep` that already fired must not send a redundant stop on drop.
#[test]
fn completed_sleep_does_not_stop() {
    let (kernel, _driver) = alarm_kernel();
    let _ = kernel.take_syscall_log();

    assert_eq!(
        crate::block_on::<fake::Syscalls, _>(
            TestAlarm::sleep_for_async(Ticks(1)).expect("frequency lookup failed")
        ),
        Ok(())
    );

    assert!(
        !kernel.take_syscall_log().iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::Command {
                driver_id: DRIVER_NUM,
                command_id: STOP,
                ..
            }
        )),
        "a Sleep that fired has no alarm left to stop"
    );
}

// -----------------------------------------------------------------------------
// Console: the allow path
// -----------------------------------------------------------------------------

use libtock_console::Console;

type TestConsole = Console<fake::Syscalls>;

const CONSOLE_DRIVER_NUM: u32 = 1;
const CONSOLE_ABORT: u32 = 3;
const CONSOLE_ALLOW_READ: u32 = 1;

#[test]
fn console_read_resolves() {
    let kernel = fake::Kernel::new();
    let driver = fake::Console::new_with_input(b"hello");
    kernel.add_driver(&driver);

    let output =
        crate::block_on::<fake::Syscalls, _>(TestConsole::read_async::<16>()).expect("read failed");

    assert_eq!(output.bytes(), b"hello");
    assert_eq!(output.count(), 5);
}

/// The property the whole owned-buffer design exists for: a read cancelled
/// mid-flight must hand the buffer back before the future's memory goes away,
/// and must not leave the console driver mid-receive.
#[test]
fn console_read_cancelled_unallows_and_aborts() {
    let kernel = fake::Kernel::new();
    let driver = fake::Console::new_with_input(b"hello");
    kernel.add_driver(&driver);
    let _ = kernel.take_syscall_log();

    {
        let read = TestConsole::read_async::<16>();
        let mut read = pin!(read);
        let mut context = Context::from_waker(Waker::noop());

        // Registers the buffer and starts the receive. No yield, so no upcall
        // can have been delivered yet.
        assert!(read.as_mut().poll(&mut context).is_pending());

        let log = kernel.take_syscall_log();
        assert!(
            log.iter().any(|entry| matches!(
                entry,
                SyscallLogEntry::AllowRw {
                    driver_num: CONSOLE_DRIVER_NUM,
                    buffer_num: CONSOLE_ALLOW_READ,
                    len: 16,
                }
            )),
            "polling should have allowed the 16-byte buffer"
        );
    }

    let log = kernel.take_syscall_log();

    assert!(
        log.iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::AllowRw {
                driver_num: CONSOLE_DRIVER_NUM,
                buffer_num: CONSOLE_ALLOW_READ,
                len: 0,
            }
        )),
        "dropping a started Read must unallow, or the kernel keeps writing into \
         memory the process is about to reuse"
    );
    assert!(
        log.iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::Command {
                driver_id: CONSOLE_DRIVER_NUM,
                command_id: CONSOLE_ABORT,
                ..
            }
        )),
        "dropping a started Read must also abort the receive"
    );
}

/// A read with no console driver present must resolve to an error, not sit in
/// `yield_wait` forever. `block_on` has no timeout: a future that goes `Pending`
/// with no upcall coming blocks the process indefinitely.
#[test]
fn console_read_without_driver_errors() {
    let _kernel = fake::Kernel::new();

    assert!(
        crate::block_on::<fake::Syscalls, _>(TestConsole::read_async::<16>()).is_err(),
        "a missing driver must surface as an error from the first poll"
    );
}
