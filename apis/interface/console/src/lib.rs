#![no_std]

use core::cell::Cell;
use core::fmt;
use core::marker::PhantomData;
use libtock_platform as platform;
use libtock_platform::allow_ro::AllowRo;
use libtock_platform::allow_rw::AllowRw;
use libtock_platform::share;
use libtock_platform::subscribe::Subscribe;
use libtock_platform::{DefaultConfig, ErrorCode, Syscalls};

/// The console driver.
///
/// It allows libraries to pass strings to the kernel's console driver.
///
/// # Example
/// ```ignore
/// use libtock::Console;
///
/// // Writes "foo", followed by a newline, to the console
/// let mut writer = Console::writer();
/// writeln!(writer, foo).unwrap();
/// ```
pub struct Console<S: Syscalls, C: Config = DefaultConfig>(S, C);

impl<S: Syscalls, C: Config> Console<S, C> {
    /// Run a check against the console capsule to ensure it is present.
    ///
    /// Returns `true` if the driver was present. This does not necessarily mean
    /// that the driver is working, as it may still fail to allocate grant
    /// memory.
    #[inline(always)]
    pub fn exists() -> bool {
        S::command(DRIVER_NUM, command::EXISTS, 0, 0).is_success()
    }

    /// Writes bytes.
    /// This is an alternative to `fmt::Write::write`
    /// because this can actually return an error code.
    pub fn write(s: &[u8]) -> Result<(), ErrorCode> {
        let called: Cell<Option<(u32,)>> = Cell::new(None);
        share::scope::<
            (
                AllowRo<_, DRIVER_NUM, { allow_ro::WRITE }>,
                Subscribe<_, DRIVER_NUM, { subscribe::WRITE }>,
            ),
            _,
            _,
        >(|handle| {
            let (allow_ro, subscribe) = handle.split();

            S::allow_ro::<C, DRIVER_NUM, { allow_ro::WRITE }>(allow_ro, s)?;

            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::WRITE }>(subscribe, &called)?;

            S::command(DRIVER_NUM, command::WRITE, s.len() as u32, 0)
                .to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((_,)) = called.get() {
                    return Ok(());
                }
            }
        })
    }

    /// Reads bytes
    /// Reads from the device and writes to `buf`, starting from index 0.
    /// No special guarantees about when the read stops.
    /// Returns count of bytes written to `buf`.
    pub fn read(buf: &mut [u8]) -> (usize, Result<(), ErrorCode>) {
        let called: Cell<Option<(u32, u32)>> = Cell::new(None);
        let mut bytes_received = 0;
        let r = share::scope::<
            (
                AllowRw<_, DRIVER_NUM, { allow_rw::READ }>,
                Subscribe<_, DRIVER_NUM, { subscribe::READ }>,
            ),
            _,
            _,
        >(|handle| {
            let (allow_rw, subscribe) = handle.split();
            let len = buf.len();
            S::allow_rw::<C, DRIVER_NUM, { allow_rw::READ }>(allow_rw, buf)?;
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::READ }>(subscribe, &called)?;

            // When this fails, `called` is guaranteed unmodified,
            // because upcalls are never processed until we call `yield`.
            S::command(DRIVER_NUM, command::READ, len as u32, 0).to_result::<(), ErrorCode>()?;

            loop {
                S::yield_wait();
                if let Some((status, bytes_pushed_count)) = called.get() {
                    bytes_received = bytes_pushed_count as usize;
                    return match status {
                        0 => Ok(()),
                        e_status => Err(e_status.try_into().unwrap_or(ErrorCode::Fail)),
                    };
                }
            }
        });
        (bytes_received, r)
    }

    pub fn writer() -> ConsoleWriter<S> {
        ConsoleWriter {
            syscalls: Default::default(),
        }
    }
}

pub struct ConsoleWriter<S: Syscalls> {
    syscalls: PhantomData<S>,
}

impl<S: Syscalls> fmt::Write for ConsoleWriter<S> {
    fn write_str(&mut self, s: &str) -> Result<(), fmt::Error> {
        Console::<S>::write(s.as_bytes()).map_err(|_e| fmt::Error)
    }
}

/// System call configuration trait for `Console`.
pub trait Config:
    platform::allow_ro::Config + platform::allow_rw::Config + platform::subscribe::Config
{
}
impl<T: platform::allow_ro::Config + platform::allow_rw::Config + platform::subscribe::Config>
    Config for T
{
}

// -----------------------------------------------------------------------------
// Async interface
// -----------------------------------------------------------------------------

/// The state the read upcall writes into: `(status, count)`.
#[cfg(feature = "async")]
#[derive(Default)]
struct ReadShared {
    result: Cell<Option<(u32, u32)>>,
    waker: Cell<Option<core::task::Waker>>,
}

#[cfg(feature = "async")]
impl platform::Upcall<platform::subscribe::AnyId> for ReadShared {
    fn upcall(&self, status: u32, count: u32, _arg2: u32) {
        self.result.set(Some((status, count)));
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

/// The bytes a completed [`Read`] produced.
#[cfg(feature = "async")]
pub struct ReadOutput<const N: usize> {
    buffer: [u8; N],
    count: usize,
}

#[cfg(feature = "async")]
impl<const N: usize> ReadOutput<N> {
    /// The bytes the kernel wrote, which may be shorter than `N`.
    pub fn bytes(&self) -> &[u8] {
        &self.buffer[..self.count]
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

/// A future that resolves once the console has delivered bytes. Create one with
/// [`Console::read_async`].
///
/// Unlike the blocking [`Console::read`], this does not borrow a buffer from the
/// caller. A future holding a `&mut [u8]` from the caller's frame could be
/// `mem::forget`-ed, which ends the borrow without running the unallow and
/// leaves the kernel writing into stack the process is free to reuse. Owning the
/// buffer converts that hazard into a leak: forgetting the future leaks the
/// memory, and memory that is never reused is memory the kernel may safely keep
/// writing to. `share::scope` avoids the same hazard with a closure, which is
/// why it cannot span an await point.
#[cfg(feature = "async")]
pub struct Read<S: Syscalls, C: Config, const N: usize> {
    // Drop order is load-bearing: `allow` unallows before `buffer` dies, and
    // `subscribe` unsubscribes before `shared` dies.
    allow: AllowRw<'static, S, DRIVER_NUM, { allow_rw::READ }>,
    subscribe: Subscribe<'static, S, DRIVER_NUM, { subscribe::READ }>,
    shared: ReadShared,
    buffer: [u8; N],
    started: bool,
    _pinned: core::marker::PhantomPinned,
    _config: PhantomData<C>,
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: Config, const N: usize> Read<S, C, N> {
    /// Reclaims the buffer from the kernel and packages the result.
    ///
    /// The unallow must happen before anything reads `buffer`: while a buffer is
    /// allowed, the kernel holds a mutable alias to it, so reading it through
    /// `&self` would alias a `&mut`.
    fn take_output(&mut self, status: u32, count: u32) -> Result<ReadOutput<N>, ErrorCode> {
        S::unallow_rw(DRIVER_NUM, allow_rw::READ);

        if status != 0 {
            return Err(status.try_into().unwrap_or(ErrorCode::Fail));
        }

        Ok(ReadOutput {
            buffer: self.buffer,
            // The kernel should never report more than it was given room for,
            // but clamp rather than trust it: this length indexes a slice.
            count: core::cmp::min(count as usize, N),
        })
    }
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: Config, const N: usize> core::future::Future for Read<S, C, N> {
    type Output = Result<ReadOutput<N>, ErrorCode>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        use core::task::Poll;

        // Safety: no field is ever moved out of the future.
        let this = unsafe { self.get_unchecked_mut() };

        if let Some((status, count)) = this.shared.result.get() {
            return Poll::Ready(this.take_output(status, count));
        }

        if !this.started {
            // Safety: both share objects live in a `!Unpin` future that has
            // already been pinned, so the pin drop guarantee runs their `Drop`
            // -- the unallow and the unsubscribe -- before this memory is
            // invalidated. That is exactly what `Handle::new` requires.
            let allow_handle = unsafe { share::Handle::new(&this.allow) };
            let subscribe_handle = unsafe { share::Handle::new(&this.subscribe) };

            // Safety: the kernel holds the buffer only until the unallow, which
            // happens in `take_output` on completion and in `AllowRw::drop`
            // otherwise. Both precede any read of `buffer` and the end of its
            // life. Nothing touches `buffer` while it is allowed.
            let buffer: &'static mut [u8] = unsafe {
                core::mem::transmute::<&mut [u8], &'static mut [u8]>(&mut this.buffer[..])
            };
            let shared: &'static ReadShared =
                unsafe { core::mem::transmute::<&ReadShared, &'static ReadShared>(&this.shared) };

            S::allow_rw::<C, DRIVER_NUM, { allow_rw::READ }>(allow_handle, buffer)?;
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::READ }>(subscribe_handle, shared)?;
            S::command(DRIVER_NUM, command::READ, N as u32, 0).to_result::<(), ErrorCode>()?;

            this.started = true;
        }

        // Only clone when the executor handed us a waker that would not wake the
        // same task. See `libtock_alarm::Sleep` for why the empty window between
        // the take and the set is safe.
        let stored = this.shared.waker.take();
        this.shared.waker.set(match stored {
            Some(waker) if waker.will_wake(context.waker()) => Some(waker),
            _ => Some(context.waker().clone()),
        });

        if let Some((status, count)) = this.shared.result.get() {
            return Poll::Ready(this.take_output(status, count));
        }

        Poll::Pending
    }
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: Config, const N: usize> Drop for Read<S, C, N> {
    /// Cancels a receive still outstanding in the kernel.
    ///
    /// Unallowing alone would leave the console driver mid-receive with nowhere
    /// to put the bytes. Command 3 aborts the receive and reports what arrived
    /// so far; that upcall is then discarded, because `Subscribe::drop` runs
    /// immediately afterwards and unsubscribing clears queued upcalls for the
    /// slot (TRD 104).
    fn drop(&mut self) {
        if self.started && self.shared.result.get().is_none() {
            // Nothing useful to do with a failure: the read is being torn down
            // either way.
            let _ = S::command(DRIVER_NUM, command::ABORT, 0, 0);
        }
    }
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: Config> Console<S, C> {
    /// Returns a future that reads up to `N` bytes from the console.
    ///
    /// `N` is chosen at the call site: `Console::<S>::read_async::<64>()`.
    pub fn read_async<const N: usize>() -> Read<S, C, N> {
        Read {
            allow: Default::default(),
            subscribe: Default::default(),
            shared: Default::default(),
            buffer: [0; N],
            started: false,
            _pinned: core::marker::PhantomPinned,
            _config: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------------
// Driver number and command IDs
// -----------------------------------------------------------------------------

const DRIVER_NUM: u32 = 0x1;

// Command IDs
#[allow(unused)]
mod command {
    pub const EXISTS: u32 = 0;
    pub const WRITE: u32 = 1;
    pub const READ: u32 = 2;
    pub const ABORT: u32 = 3;
}

#[allow(unused)]
mod subscribe {
    pub const WRITE: u32 = 1;
    pub const READ: u32 = 2;
}

mod allow_ro {
    pub const WRITE: u32 = 1;
}

mod allow_rw {
    pub const READ: u32 = 1;
}
