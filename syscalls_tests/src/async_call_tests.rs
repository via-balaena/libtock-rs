//! Direct tests for `libtock_platform::async_call`.
//!
//! The primitives are exercised here against a driver written for the purpose,
//! rather than through `libtock_alarm` or `libtock_console`, so that a
//! regression in `Call` or `BufferedCall` fails in the crate that owns them.

use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::{pin, Pin};
use core::task::{Context, Poll, Waker};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::Wake;

use libtock_platform::async_call::{BufferedCall, BufferedOperation, Operation};
use libtock_platform::{CommandReturn, DefaultConfig, ErrorCode, Syscalls};
use libtock_unittest::{fake, DriverInfo, DriverShareRef, RwAllowBuffer, SyscallLogEntry};

const DRIVER_NUM: u32 = 42;
const SUBSCRIBE_NUM: u32 = 0;
const BUFFER_NUM: u32 = 0;

const START: u32 = 1;
const CANCEL: u32 = 2;

/// What the driver writes into an allowed buffer.
const PAYLOAD: &[u8] = b"hi";

/// The arguments the driver reports through its upcall. The middle field is the
/// byte count, so it is derived from `PAYLOAD` rather than restated: the two
/// must agree, and a test that pinned them independently would fail confusingly
/// when only one changed.
const UPCALL_ARGS: (u32, u32, u32) = (7, PAYLOAD.len() as u32, 9);

// -----------------------------------------------------------------------------
// A driver that exists only to drive the primitives
// -----------------------------------------------------------------------------

#[derive(Default)]
struct TestDriver {
    share_ref: DriverShareRef,
    buffer: RefCell<RwAllowBuffer>,
    /// When set, START answers with a failure instead of scheduling an upcall.
    fail_start: Cell<bool>,
}

impl TestDriver {
    fn new() -> std::rc::Rc<TestDriver> {
        std::rc::Rc::new(Default::default())
    }

    fn failing() -> std::rc::Rc<TestDriver> {
        let driver = Self::new();
        driver.fail_start.set(true);
        driver
    }
}

impl fake::SyscallDriver for TestDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo::new(DRIVER_NUM).upcall_count(1)
    }

    fn register(&self, share_ref: DriverShareRef) {
        self.share_ref.replace(share_ref);
    }

    fn command(&self, command_id: u32, _argument0: u32, _argument1: u32) -> CommandReturn {
        match command_id {
            START if self.fail_start.get() => {
                libtock_unittest::command_return::failure(ErrorCode::Fail)
            }
            START => {
                // Write into the allowed buffer, if there is one, so buffered
                // tests can prove the bytes survive the hand-back.
                let mut buffer = self.buffer.borrow_mut();
                if buffer.len() >= PAYLOAD.len() {
                    buffer[..PAYLOAD.len()].copy_from_slice(PAYLOAD);
                }
                self.share_ref
                    .schedule_upcall(SUBSCRIBE_NUM, UPCALL_ARGS)
                    .expect("schedule_upcall failed");
                libtock_unittest::command_return::success()
            }
            CANCEL => libtock_unittest::command_return::success(),
            _ => libtock_unittest::command_return::failure(ErrorCode::NoSupport),
        }
    }

    fn allow_readwrite(
        &self,
        buffer_num: u32,
        buffer: RwAllowBuffer,
    ) -> Result<RwAllowBuffer, (RwAllowBuffer, ErrorCode)> {
        if buffer_num == BUFFER_NUM {
            Ok(self.buffer.replace(buffer))
        } else {
            Err((buffer, ErrorCode::Invalid))
        }
    }
}

// -----------------------------------------------------------------------------
// Test operations
// -----------------------------------------------------------------------------

/// Records what `Call` asked of it, so the tests can assert on the primitive's
/// behavior rather than on syscall side effects alone.
#[derive(Default)]
struct Journal {
    starts: Cell<u32>,
    cancels: Cell<u32>,
    completed_with: Cell<Option<(u32, u32, u32)>>,
    start_len: Cell<Option<usize>>,
    completed_bytes: RefCell<Vec<u8>>,
}

struct TestOp<'a> {
    journal: &'a Journal,
}

impl<S: Syscalls> Operation<S> for TestOp<'_> {
    type Value = (u32, u32, u32);

    fn start(&self) -> Result<(), ErrorCode> {
        self.journal.starts.set(self.journal.starts.get() + 1);
        S::command(DRIVER_NUM, START, 0, 0).to_result::<(), ErrorCode>()
    }

    fn cancel(&self) {
        self.journal.cancels.set(self.journal.cancels.get() + 1);
        let _ = S::command(DRIVER_NUM, CANCEL, 0, 0);
    }

    fn complete(&self, args: (u32, u32, u32)) -> Result<Self::Value, ErrorCode> {
        self.journal.completed_with.set(Some(args));
        Ok(args)
    }
}

struct TestBufferedOp<'a> {
    journal: &'a Journal,
}

impl<S: Syscalls, const N: usize> BufferedOperation<S, N> for TestBufferedOp<'_> {
    type Value = usize;

    fn start(&self, len: usize) -> Result<(), ErrorCode> {
        self.journal.starts.set(self.journal.starts.get() + 1);
        self.journal.start_len.set(Some(len));
        S::command(DRIVER_NUM, START, 0, 0).to_result::<(), ErrorCode>()
    }

    fn cancel(&self) {
        self.journal.cancels.set(self.journal.cancels.get() + 1);
        let _ = S::command(DRIVER_NUM, CANCEL, 0, 0);
    }

    fn complete(&self, args: (u32, u32, u32), buffer: &[u8; N]) -> Result<Self::Value, ErrorCode> {
        // Reading `buffer` here is only sound because `BufferedCall` unallowed
        // first. If it ever stops doing so, the fake kernel still holds a
        // `&mut` alias to these bytes and Miri fails this test.
        self.journal.completed_bytes.replace(buffer.to_vec());
        Ok(args.1 as usize)
    }
}

type TestCall<'a> = libtock_platform::async_call::Call<
    fake::Syscalls,
    DefaultConfig,
    TestOp<'a>,
    DRIVER_NUM,
    SUBSCRIBE_NUM,
>;

/// Eight bytes is comfortably larger than `PAYLOAD`, so the buffered tests cover
/// a short read rather than an exactly-filled buffer.
type TestBufferedCall<'a> = BufferedCall<
    fake::Syscalls,
    DefaultConfig,
    TestBufferedOp<'a>,
    DRIVER_NUM,
    SUBSCRIBE_NUM,
    BUFFER_NUM,
    8,
>;

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

struct CountingWaker(AtomicU32);

impl Wake for CountingWaker {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Polls to completion, yielding in between so the fake kernel can deliver its
/// upcall. Deliberately not `libtock_async::block_on`: these tests should fail
/// when the primitive breaks, not when the executor does.
///
/// This cannot hang a test run. The fake kernel panics on a `yield_wait` with no
/// upcall pending -- "friendlier than hanging" -- so a future that never
/// resolves fails loudly instead of blocking forever.
fn drive<F: Future>(mut future: Pin<&mut F>, waker: &Waker) -> F::Output {
    let mut context = Context::from_waker(waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        fake::Syscalls::yield_wait();
    }
}

fn subscribe_count(log: &[SyscallLogEntry]) -> usize {
    log.iter()
        .filter(|entry| {
            matches!(
                entry,
                SyscallLogEntry::Subscribe {
                    driver_num: DRIVER_NUM,
                    subscribe_num: SUBSCRIBE_NUM,
                }
            )
        })
        .count()
}

fn command_count(log: &[SyscallLogEntry], id: u32) -> usize {
    log.iter()
        .filter(|entry| match entry {
            SyscallLogEntry::Command {
                driver_id: DRIVER_NUM,
                command_id,
                ..
            } => *command_id == id,
            _ => false,
        })
        .count()
}

// -----------------------------------------------------------------------------
// Call
// -----------------------------------------------------------------------------

#[test]
fn upcall_arguments_reach_complete() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();
    let call = pin!(TestCall::new(TestOp { journal: &journal }));

    assert_eq!(drive(call, &waker), Ok(UPCALL_ARGS));
    assert_eq!(journal.completed_with.get(), Some(UPCALL_ARGS));
}

/// `block_on` uses a no-op waker, so nothing in the driver crates would notice
/// if the upcall stopped waking anyone. Under a real executor that is a hang.
#[test]
fn upcall_wakes_the_stored_waker() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);

    let journal = Journal::default();
    let counter = Arc::new(CountingWaker(AtomicU32::new(0)));
    let waker: Waker = counter.clone().into();
    let call = pin!(TestCall::new(TestOp { journal: &journal }));

    assert!(drive(call, &waker).is_ok());
    assert!(
        counter.0.load(Ordering::SeqCst) >= 1,
        "the upcall must wake the waker the future was polled with"
    );
}

#[test]
fn repeated_polls_do_not_restart_the_operation() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);
    let _ = kernel.take_syscall_log();

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();
    let mut context = Context::from_waker(&waker);
    let mut call = pin!(TestCall::new(TestOp { journal: &journal }));

    assert!(call.as_mut().poll(&mut context).is_pending());
    assert!(call.as_mut().poll(&mut context).is_pending());

    assert_eq!(journal.starts.get(), 1, "start must run exactly once");
    assert_eq!(
        subscribe_count(&kernel.take_syscall_log()),
        1,
        "a second poll must not re-register the upcall"
    );
}

#[test]
fn start_failure_surfaces_and_skips_cancel() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::failing();
    kernel.add_driver(&driver);
    let _ = kernel.take_syscall_log();

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();
    let mut context = Context::from_waker(&waker);

    {
        let mut call = pin!(TestCall::new(TestOp { journal: &journal }));
        assert_eq!(
            call.as_mut().poll(&mut context),
            Poll::Ready(Err(ErrorCode::Fail))
        );
    }

    assert_eq!(
        journal.cancels.get(),
        0,
        "an operation that never started has nothing to cancel"
    );
    // `subscribe` succeeded before `start` failed, so a registration is live at
    // the point the future resolves with an error. This is the one error path
    // where the teardown the whole design rests on was not being checked.
    assert_eq!(
        subscribe_count(&kernel.take_syscall_log()),
        2,
        "a failed start must still leave the subscription unregistered on drop"
    );
}

#[test]
fn cancel_runs_once_when_dropped_outstanding() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);
    let _ = kernel.take_syscall_log();

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();
    let mut context = Context::from_waker(&waker);

    {
        let mut call = pin!(TestCall::new(TestOp { journal: &journal }));
        assert!(call.as_mut().poll(&mut context).is_pending());
    }

    assert_eq!(journal.cancels.get(), 1);
    let log = kernel.take_syscall_log();
    assert_eq!(command_count(&log, CANCEL), 1);
    // Two entries: the registration from `poll`, then the unregistration from
    // `Subscribe::drop`. Unsubscribe is Subscribe-with-null, so the log cannot
    // tell them apart by kind -- the count is the signal.
    assert_eq!(
        subscribe_count(&log),
        2,
        "drop must unsubscribe after the cancel, leaving register + unregister"
    );
}

#[test]
fn cancel_skipped_when_never_polled() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);

    let journal = Journal::default();
    drop(TestCall::new(TestOp { journal: &journal }));

    assert_eq!(journal.starts.get(), 0);
    assert_eq!(journal.cancels.get(), 0);
}

#[test]
fn cancel_skipped_after_completion() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();

    {
        let call = pin!(TestCall::new(TestOp { journal: &journal }));
        assert!(drive(call, &waker).is_ok());
    }

    assert_eq!(
        journal.cancels.get(),
        0,
        "a completed operation has nothing to cancel"
    );
}

// -----------------------------------------------------------------------------
// BufferedCall
// -----------------------------------------------------------------------------

#[test]
fn buffered_call_hands_back_the_written_buffer() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);
    let _ = kernel.take_syscall_log();

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();
    let call = pin!(TestBufferedCall::new(TestBufferedOp { journal: &journal }));

    assert_eq!(drive(call, &waker), Ok(PAYLOAD.len()));
    assert_eq!(journal.start_len.get(), Some(8), "start receives N");
    assert_eq!(&journal.completed_bytes.borrow()[..PAYLOAD.len()], PAYLOAD);

    assert!(
        kernel.take_syscall_log().iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::AllowRw {
                driver_num: DRIVER_NUM,
                buffer_num: BUFFER_NUM,
                len: 0,
            }
        )),
        "the buffer must be unallowed before complete reads it"
    );
}

#[test]
fn buffered_call_cancelled_unallows_and_cancels() {
    let kernel = fake::Kernel::new();
    let driver = TestDriver::new();
    kernel.add_driver(&driver);
    let _ = kernel.take_syscall_log();

    let journal = Journal::default();
    let waker: Waker = Arc::new(CountingWaker(AtomicU32::new(0))).into();
    let mut context = Context::from_waker(&waker);

    {
        let mut call = pin!(TestBufferedCall::new(TestBufferedOp { journal: &journal }));
        assert!(call.as_mut().poll(&mut context).is_pending());
    }

    let log = kernel.take_syscall_log();
    assert_eq!(journal.cancels.get(), 1);
    assert!(
        log.iter().any(|entry| matches!(
            entry,
            SyscallLogEntry::AllowRw {
                driver_num: DRIVER_NUM,
                buffer_num: BUFFER_NUM,
                len: 0,
            }
        )),
        "drop must unallow"
    );
    assert_eq!(
        subscribe_count(&log),
        2,
        "drop must unsubscribe, leaving register + unregister"
    );
}
