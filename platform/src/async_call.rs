//! Futures over Tock system calls.
//!
//! The two primitives here exist so that the reasoning which makes a `Future`
//! over an upcall sound lives in exactly one audited place. That reasoning has
//! three parts, and all three are easy to get subtly wrong in a driver:
//!
//! 1. The share objects must live *inside* the future. `share::scope` cannot
//!    span an await point, because its closure drops the share list on return.
//!    A future holding a `&mut [u8]` borrowed from the caller is worse than
//!    useless: `mem::forget` would end the borrow without running the unallow,
//!    leaving the kernel writing into memory the process may reuse.
//! 2. They must drop in an order that tears down the kernel's view before the
//!    memory it points at. Field order below is load-bearing.
//! 3. The reference handed to the kernel must be widened to `'static`, which is
//!    sound only because of 1 and 2 together.
//!
//! Drivers built on these contain no `unsafe`.

use core::cell::Cell;
use core::marker::{PhantomData, PhantomPinned};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::share;
use crate::subscribe::AnyId;
use crate::{AllowRw, ErrorCode, Subscribe, Syscalls, Upcall};

/// The driver-specific half of a [`Call`].
///
/// An implementation is a plain data struct plus three short methods; it never
/// touches `Subscribe`, lifetimes, or `Pin`.
pub trait Operation<S: Syscalls> {
    /// What this operation produces on success.
    type Value;

    /// Issues the command that starts the operation. Run once, after the upcall
    /// has been registered.
    fn start(&self) -> Result<(), ErrorCode>;

    /// Cancels an operation that is still outstanding, called from `Drop` when
    /// the upcall never arrived. Implement as a no-op for drivers with nothing
    /// to cancel.
    ///
    /// Any upcall this provokes is discarded: the unsubscribe that follows
    /// immediately clears queued upcalls for the slot (TRD 104).
    fn cancel(&self);

    /// Interprets the upcall's arguments.
    fn complete(&self, args: (u32, u32, u32)) -> Result<Self::Value, ErrorCode>;
}

/// The driver-specific half of a [`BufferedCall`].
pub trait BufferedOperation<S: Syscalls, const N: usize> {
    /// What this operation produces on success.
    type Value;

    /// Issues the command that starts the operation, given the length of the
    /// buffer that has just been shared with the kernel.
    fn start(&self, len: usize) -> Result<(), ErrorCode>;

    /// Cancels an operation that is still outstanding. See
    /// [`Operation::cancel`].
    fn cancel(&self);

    /// Interprets the upcall's arguments. The buffer has already been unallowed,
    /// so it is exclusively the process's again.
    fn complete(&self, args: (u32, u32, u32), buffer: &[u8; N]) -> Result<Self::Value, ErrorCode>;
}

/// The state an upcall writes into, shared between the kernel and `poll`.
#[derive(Default)]
struct Shared {
    args: Cell<Option<(u32, u32, u32)>>,
    waker: Cell<Option<Waker>>,
}

impl Upcall<AnyId> for Shared {
    fn upcall(&self, arg0: u32, arg1: u32, arg2: u32) {
        self.args.set(Some((arg0, arg1, arg2)));
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

impl Shared {
    /// Stores `waker` unless the stored one would already wake the same task.
    ///
    /// The take/put round trip leaves the cell briefly empty. That is safe
    /// because Tock delivers upcalls only inside a Yield system call, so one
    /// cannot land here; and callers re-read `args` afterwards, which would
    /// catch it even if one could.
    fn store_waker(&self, waker: &Waker) {
        let stored = self.waker.take();
        self.waker.set(match stored {
            Some(existing) if existing.will_wake(waker) => Some(existing),
            _ => Some(waker.clone()),
        });
    }
}

/// A future over one subscribe-and-wait operation.
///
/// Use it through a type alias, which fixes the driver and subscribe numbers:
///
/// ```ignore
/// pub type Sleep<S, C = DefaultConfig> = Call<S, C, SleepOp, DRIVER_NUM, CALLBACK>;
/// ```
pub struct Call<
    S: Syscalls,
    C: crate::subscribe::Config,
    O: Operation<S>,
    const DRIVER_NUM: u32,
    const SUBSCRIBE_NUM: u32,
> {
    // Declared before `shared` so it drops first: `Subscribe::drop`
    // unsubscribes, which must happen while the upcall target is still valid.
    subscribe: Subscribe<'static, S, DRIVER_NUM, SUBSCRIBE_NUM>,
    shared: Shared,
    operation: O,
    started: bool,
    // `poll` hands the kernel a pointer into `shared`, so this must not move.
    _pinned: PhantomPinned,
    _config: PhantomData<C>,
}

impl<S: Syscalls, C: crate::subscribe::Config, O: Operation<S>, const D: u32, const N: u32>
    Call<S, C, O, D, N>
{
    pub fn new(operation: O) -> Self {
        Call {
            subscribe: Default::default(),
            shared: Default::default(),
            operation,
            started: false,
            _pinned: PhantomPinned,
            _config: PhantomData,
        }
    }
}

impl<S: Syscalls, C: crate::subscribe::Config, O: Operation<S>, const D: u32, const N: u32>
    core::future::Future for Call<S, C, O, D, N>
{
    type Output = Result<O::Value, ErrorCode>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: no field is ever moved out of the future.
        let this = unsafe { self.get_unchecked_mut() };

        if let Some(args) = this.shared.args.get() {
            return Poll::Ready(this.operation.complete(args));
        }

        if !this.started {
            // Safety: `Handle::new` requires the list be dropped rather than
            // forgotten. `subscribe` sits in a `!Unpin` future that has already
            // been pinned -- `poll` cannot be reached otherwise -- so the pin
            // drop guarantee provides exactly that.
            let handle = unsafe { share::Handle::new(&this.subscribe) };

            // Safety: the kernel retains this pointer only until the matching
            // unsubscribe, which `Subscribe::drop` performs before `shared` is
            // invalidated, per the field ordering above.
            let shared: &'static Shared =
                unsafe { core::mem::transmute::<&Shared, &'static Shared>(&this.shared) };

            S::subscribe::<_, _, C, D, N>(handle, shared)?;
            this.operation.start()?;
            this.started = true;
        }

        this.shared.store_waker(context.waker());

        if let Some(args) = this.shared.args.get() {
            return Poll::Ready(this.operation.complete(args));
        }

        Poll::Pending
    }
}

impl<S: Syscalls, C: crate::subscribe::Config, O: Operation<S>, const D: u32, const N: u32> Drop
    for Call<S, C, O, D, N>
{
    fn drop(&mut self) {
        if self.started && self.shared.args.get().is_none() {
            self.operation.cancel();
        }
    }
}

/// A future over one allow-subscribe-and-wait operation, owning the buffer the
/// kernel writes into.
pub struct BufferedCall<
    S: Syscalls,
    C: crate::subscribe::Config + crate::allow_rw::Config,
    O: BufferedOperation<S, N>,
    const DRIVER_NUM: u32,
    const SUBSCRIBE_NUM: u32,
    const BUFFER_NUM: u32,
    const N: usize,
> {
    // Drop order is load-bearing: `allow` unallows before `buffer` dies, and
    // `subscribe` unsubscribes before `shared` dies.
    allow: AllowRw<'static, S, DRIVER_NUM, BUFFER_NUM>,
    subscribe: Subscribe<'static, S, DRIVER_NUM, SUBSCRIBE_NUM>,
    shared: Shared,
    buffer: [u8; N],
    operation: O,
    started: bool,
    _pinned: PhantomPinned,
    _config: PhantomData<C>,
}

impl<
        S: Syscalls,
        C: crate::subscribe::Config + crate::allow_rw::Config,
        O: BufferedOperation<S, N>,
        const D: u32,
        const SUB: u32,
        const BUF: u32,
        const N: usize,
    > BufferedCall<S, C, O, D, SUB, BUF, N>
{
    pub fn new(operation: O) -> Self {
        BufferedCall {
            allow: Default::default(),
            subscribe: Default::default(),
            shared: Default::default(),
            buffer: [0; N],
            operation,
            started: false,
            _pinned: PhantomPinned,
            _config: PhantomData,
        }
    }

    /// Reclaims the buffer from the kernel, then interprets the result.
    ///
    /// The unallow must precede any read of `buffer`: while a buffer is allowed
    /// the kernel holds a mutable alias to it, so reading it through `&self`
    /// would alias a `&mut`.
    fn finish(&self, args: (u32, u32, u32)) -> Result<O::Value, ErrorCode> {
        S::unallow_rw(D, BUF);
        self.operation.complete(args, &self.buffer)
    }
}

impl<
        S: Syscalls,
        C: crate::subscribe::Config + crate::allow_rw::Config,
        O: BufferedOperation<S, N>,
        const D: u32,
        const SUB: u32,
        const BUF: u32,
        const N: usize,
    > core::future::Future for BufferedCall<S, C, O, D, SUB, BUF, N>
{
    type Output = Result<O::Value, ErrorCode>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: no field is ever moved out of the future.
        let this = unsafe { self.get_unchecked_mut() };

        if let Some(args) = this.shared.args.get() {
            return Poll::Ready(this.finish(args));
        }

        if !this.started {
            // Safety: both share objects live in a `!Unpin` future that has
            // already been pinned, so the pin drop guarantee runs their `Drop`
            // -- the unallow and the unsubscribe -- before this memory is
            // invalidated. That is what `Handle::new` requires.
            let allow_handle = unsafe { share::Handle::new(&this.allow) };
            let subscribe_handle = unsafe { share::Handle::new(&this.subscribe) };

            // Safety: the kernel holds the buffer only until the unallow, which
            // happens in `finish` on completion and in `AllowRw::drop`
            // otherwise. Both precede any read of `buffer` and the end of its
            // life; nothing reads `buffer` while it is allowed.
            let buffer: &'static mut [u8] = unsafe {
                core::mem::transmute::<&mut [u8], &'static mut [u8]>(&mut this.buffer[..])
            };
            let shared: &'static Shared =
                unsafe { core::mem::transmute::<&Shared, &'static Shared>(&this.shared) };

            S::allow_rw::<C, D, BUF>(allow_handle, buffer)?;
            S::subscribe::<_, _, C, D, SUB>(subscribe_handle, shared)?;
            this.operation.start(N)?;
            this.started = true;
        }

        this.shared.store_waker(context.waker());

        if let Some(args) = this.shared.args.get() {
            return Poll::Ready(this.finish(args));
        }

        Poll::Pending
    }
}

impl<
        S: Syscalls,
        C: crate::subscribe::Config + crate::allow_rw::Config,
        O: BufferedOperation<S, N>,
        const D: u32,
        const SUB: u32,
        const BUF: u32,
        const N: usize,
    > Drop for BufferedCall<S, C, O, D, SUB, BUF, N>
{
    fn drop(&mut self) {
        if self.started && self.shared.args.get().is_none() {
            self.operation.cancel();
        }
    }
}
