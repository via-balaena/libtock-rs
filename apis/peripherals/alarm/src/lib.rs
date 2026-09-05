#![no_std]

use core::cell::Cell;
use libtock_platform as platform;
use libtock_platform::share;
use libtock_platform::{DefaultConfig, ErrorCode, Syscalls};

/// The alarm driver
///
/// # Example
/// ```ignore
/// use libtock2::Alarm;
///
/// // Wait for timeout
/// Alarm::sleep(Alarm::Milliseconds(2500));
/// ```
pub struct Alarm<S: Syscalls, C: platform::subscribe::Config = DefaultConfig>(S, C);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Hz(pub u32);

pub trait Convert {
    /// Converts a time unit by rounding up.
    fn to_ticks(self, freq: Hz) -> Ticks;
}

#[derive(Copy, Clone, Debug)]
pub struct Ticks(pub u32);

impl Convert for Ticks {
    fn to_ticks(self, _freq: Hz) -> Ticks {
        self
    }
}

#[derive(Copy, Clone)]
pub struct Milliseconds(pub u32);

impl Convert for Milliseconds {
    fn to_ticks(self, freq: Hz) -> Ticks {
        // Saturating multiplication will top out at about 1 hour at 1MHz.
        // It's large enough for an alarm, and much simpler than failing
        // or losing precision for short sleeps.

        /// u32::div_ceil is still unstable.
        fn div_ceil(a: u32, other: u32) -> u32 {
            let d = a / other;
            let m = a % other;
            if m == 0 {
                d
            } else {
                d + 1
            }
        }
        Ticks(div_ceil(self.0.saturating_mul(freq.0), 1000))
    }
}

impl<S: Syscalls, C: platform::subscribe::Config> Alarm<S, C> {
    /// Run a check against the console capsule to ensure it is present.
    #[inline(always)]
    pub fn exists() -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, command::EXISTS, 0, 0).to_result()
    }

    pub fn get_frequency() -> Result<Hz, ErrorCode> {
        S::command(DRIVER_NUM, command::FREQUENCY, 0, 0)
            .to_result()
            .map(Hz)
    }

    pub fn get_ticks() -> Result<u32, ErrorCode> {
        S::command(DRIVER_NUM, command::TIME, 0, 0).to_result()
    }

    pub fn get_milliseconds() -> Result<u64, ErrorCode> {
        let ticks = Self::get_ticks()? as u64;
        let freq = (Self::get_frequency()?).0 as u64;

        Ok(ticks.saturating_div(freq / 1000))
    }

    pub fn sleep_for<T: Convert>(time: T) -> Result<(), ErrorCode> {
        let freq = Self::get_frequency()?;
        let ticks = time.to_ticks(freq);

        let called: Cell<Option<(u32, u32)>> = Cell::new(None);
        share::scope(|subscribe| {
            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::CALLBACK }>(subscribe, &called)?;

            S::command(DRIVER_NUM, command::SET_RELATIVE, ticks.0, 0)
                .to_result()
                .map(|_when: u32| ())?;

            loop {
                S::yield_wait();
                if let Some((_when, _ref)) = called.get() {
                    return Ok(());
                }
            }
        })
    }
}

// -----------------------------------------------------------------------------
// Async interface
// -----------------------------------------------------------------------------

/// The state the alarm upcall writes into. This lives inside the [`Sleep`]
/// future rather than in a `static`, so each future owns its own completion
/// state and the pattern generalizes to drivers that support several
/// outstanding operations.
#[cfg(feature = "async")]
#[derive(Default)]
struct SleepShared {
    fired: Cell<bool>,
    waker: Cell<Option<core::task::Waker>>,
}

#[cfg(feature = "async")]
impl platform::Upcall<platform::subscribe::AnyId> for SleepShared {
    fn upcall(&self, _when: u32, _ref: u32, _arg2: u32) {
        self.fired.set(true);
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

/// A future that resolves once the alarm fires. Create one with
/// [`Alarm::sleep_for_async`].
///
/// The subscription is owned by the future, so dropping it — which is what
/// `select` does to the loser of a race — unsubscribes before the memory the
/// kernel would write into goes away. That is the whole reason the state is not
/// borrowed from the caller's stack.
///
/// Only one `Sleep` may be polled at a time: Tock gives a process a single
/// upcall slot per (driver, subscribe number), so a second `Sleep` polled while
/// the first is outstanding would silently replace the first one's
/// registration. Awaiting them in sequence is fine; racing two of them is not.
/// Multiplexing one alarm across several deadlines needs a driver-level queue,
/// the same problem `embassy-time-driver` solves.
#[cfg(feature = "async")]
pub struct Sleep<S: Syscalls, C: platform::subscribe::Config = DefaultConfig> {
    // Declared before `shared` so that it drops first: `Subscribe::drop` issues
    // the unsubscribe, which must happen while `shared` is still valid. The
    // field order here is load-bearing, not cosmetic.
    subscribe: platform::Subscribe<'static, S, DRIVER_NUM, { subscribe::CALLBACK }>,
    shared: SleepShared,
    ticks: Ticks,
    started: bool,
    // Polling hands the kernel a pointer into `shared`, so the future must not
    // move afterwards.
    _pinned: core::marker::PhantomPinned,
    _config: core::marker::PhantomData<C>,
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: platform::subscribe::Config> core::future::Future for Sleep<S, C> {
    type Output = Result<(), ErrorCode>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        context: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        use core::task::Poll;

        // Safety: no field is ever moved out of the future.
        let this = unsafe { self.get_unchecked_mut() };

        if this.shared.fired.get() {
            return Poll::Ready(Ok(()));
        }

        if !this.started {
            // Safety: `Handle::new` requires that the list be dropped rather
            // than forgotten or leaked. `subscribe` sits inside a `!Unpin`
            // future that has already been pinned, so the pin drop guarantee
            // provides exactly that.
            let handle = unsafe { share::Handle::new(&this.subscribe) };

            // Safety: the kernel retains this pointer only until the matching
            // unsubscribe, which `Subscribe::drop` performs before `shared` is
            // invalidated (see the field ordering above). Widening the borrow to
            // 'static is what allows the upcall target to live in the future
            // instead of in a `static`.
            let shared: &'static SleepShared =
                unsafe { core::mem::transmute::<&SleepShared, &'static SleepShared>(&this.shared) };

            S::subscribe::<_, _, C, DRIVER_NUM, { subscribe::CALLBACK }>(handle, shared)?;

            S::command(DRIVER_NUM, command::SET_RELATIVE, this.ticks.0, 0)
                .to_result()
                .map(|_when: u32| ())?;

            this.started = true;
        }

        // Only clone when the executor handed us a waker that would not wake the
        // same task. `Cell` cannot inspect in place, hence the take/put round
        // trip — which leaves the cell empty in between. That window is safe for
        // the same reason the re-check below is: Tock delivers upcalls only
        // inside Yield, so one cannot land here, and an upcall that somehow did
        // would still be caught by the `fired` read that follows.
        let stored = this.shared.waker.take();
        this.shared.waker.set(match stored {
            Some(waker) if waker.will_wake(context.waker()) => Some(waker),
            _ => Some(context.waker().clone()),
        });

        // Re-check. Nothing can have fired since the read at the top of `poll`,
        // per the invariant above, but this costs a single load and keeps the
        // future correct under any executor.
        if this.shared.fired.get() {
            return Poll::Ready(Ok(()));
        }

        Poll::Pending
    }
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: platform::subscribe::Config> Drop for Sleep<S, C> {
    /// Cancels the alarm still pending in the kernel.
    ///
    /// Unsubscribing alone would leave the alarm capsule holding a virtual
    /// alarm for this process that expires into a null upcall. This body runs
    /// before the fields drop, so the alarm is stopped before `Subscribe::drop`
    /// unsubscribes.
    fn drop(&mut self) {
        if self.started && !self.shared.fired.get() {
            // There is nothing useful to do with a failure here: the operation
            // is already being torn down, and a stop that does not land only
            // wastes kernel state.
            let _ = S::command(DRIVER_NUM, command::STOP, 0, 0);
        }
    }
}

#[cfg(feature = "async")]
impl<S: Syscalls, C: platform::subscribe::Config> Alarm<S, C> {
    /// Returns a future that resolves after `time` has elapsed.
    ///
    /// The frequency lookup happens here rather than on first poll, so polling
    /// cannot fail for a reason the caller has not already had a chance to see.
    pub fn sleep_for_async<T: Convert>(time: T) -> Result<Sleep<S, C>, ErrorCode> {
        let freq = Self::get_frequency()?;

        Ok(Sleep {
            subscribe: Default::default(),
            shared: Default::default(),
            ticks: time.to_ticks(freq),
            started: false,
            _pinned: core::marker::PhantomPinned,
            _config: core::marker::PhantomData,
        })
    }
}

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------------
// Driver number and command IDs
// -----------------------------------------------------------------------------

const DRIVER_NUM: u32 = 0x0;

// Command IDs
#[allow(unused)]
mod command {
    pub const EXISTS: u32 = 0;
    pub const FREQUENCY: u32 = 1;
    pub const TIME: u32 = 2;
    pub const STOP: u32 = 3;

    pub const SET_RELATIVE: u32 = 5;
    pub const SET_ABSOLUTE: u32 = 6;
}

#[allow(unused)]
mod subscribe {
    pub const CALLBACK: u32 = 0;
}
