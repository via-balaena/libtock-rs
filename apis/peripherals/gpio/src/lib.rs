#![no_std]

use core::marker::PhantomData;

use libtock_platform::{
    share::Handle, subscribe::OneId, DefaultConfig, ErrorCode, Subscribe, Syscalls, Upcall,
};

/// The GPIO driver.
///
/// # Example
/// ```ignore
/// use libtock::gpio;
///
/// // Set pin to high.
/// let pin = gpio::Gpio::get_pin(0).unwrap().make_output().unwrap();
/// let _ = pin.set();
/// ```
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum GpioState {
    Low = 0,
    High = 1,
}

pub enum PinInterruptEdge {
    Either = 0,
    Rising = 1,
    Falling = 2,
}

pub enum Error {
    Invalid,
    Failed,
}

pub trait Pull {
    const MODE: u32;
}

pub struct PullUp;
impl Pull for PullUp {
    const MODE: u32 = 1;
}

pub struct PullDown;
impl Pull for PullDown {
    const MODE: u32 = 2;
}

pub struct PullNone;
impl Pull for PullNone {
    const MODE: u32 = 0;
}

pub struct Gpio<S: Syscalls>(S);

impl<S: Syscalls> Gpio<S> {
    /// Returns Ok() if the driver was present.This does not necessarily mean
    /// that the driver is working, as it may still fail to allocate grant
    /// memory.
    pub fn exists() -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, EXISTS, 0, 0).to_result()
    }

    pub fn count() -> Result<u32, ErrorCode> {
        S::command(DRIVER_NUM, GPIO_COUNT, 0, 0).to_result()
    }

    pub fn get_pin(pin: u32) -> Result<Pin<S>, ErrorCode> {
        Self::disable(pin)?;
        Ok(Pin {
            pin_number: pin,
            _syscalls: PhantomData,
        })
    }

    /// Register an interrupt listener
    ///
    /// There can be only one single listener registered at a time.
    /// Each time this function is used, it will replace the
    /// previously registered listener.
    pub fn register_listener<'share, F: Fn(u32, GpioState)>(
        listener: &'share GpioInterruptListener<F>,
        subscribe: Handle<Subscribe<'share, S, DRIVER_NUM, INTERRUPT_UPCALL>>,
    ) -> Result<(), ErrorCode> {
        S::subscribe::<_, _, DefaultConfig, DRIVER_NUM, INTERRUPT_UPCALL>(subscribe, listener)
    }

    /// Unregister the interrupt listener
    ///
    /// This function may be used even if there was no
    /// previously registered listener.
    pub fn unregister_listener() {
        S::unsubscribe(DRIVER_NUM, INTERRUPT_UPCALL)
    }
}

/// A wrapper around a closure to be registered and called when
/// a gpio interrupt occurs.
///
/// ```ignore
/// let listener = GpioInterruptListener(|gpio, interrupt_edge| {
///     // make use of the button's state
/// });
/// ```
pub struct GpioInterruptListener<F: Fn(u32, GpioState)>(pub F);

impl<F: Fn(u32, GpioState)> Upcall<OneId<DRIVER_NUM, INTERRUPT_UPCALL>>
    for GpioInterruptListener<F>
{
    fn upcall(&self, gpio_index: u32, value: u32, _arg2: u32) {
        self.0(gpio_index, value.into())
    }
}

impl From<u32> for GpioState {
    fn from(original: u32) -> GpioState {
        match original {
            0 => GpioState::Low,
            _ => GpioState::High,
        }
    }
}

pub struct Pin<S: Syscalls> {
    pin_number: u32,
    _syscalls: PhantomData<S>,
}

impl<S: Syscalls> Pin<S> {
    pub fn make_output(&mut self) -> Result<OutputPin<'_, S>, ErrorCode> {
        Gpio::<S>::enable_gpio_output(self.pin_number)?;
        Ok(OutputPin { pin: self })
    }

    pub fn make_input<P: Pull>(&self) -> Result<InputPin<'_, S, P>, ErrorCode> {
        Gpio::<S>::enable_gpio_input(self.pin_number, P::MODE)?;
        Ok(InputPin {
            pin: self,
            _pull: PhantomData,
        })
    }
}

pub struct OutputPin<'a, S: Syscalls> {
    pin: &'a Pin<S>,
}

impl<S: Syscalls> OutputPin<'_, S> {
    pub fn toggle(&mut self) -> Result<(), ErrorCode> {
        Gpio::<S>::toggle(self.pin.pin_number)
    }
    pub fn set(&mut self) -> Result<(), ErrorCode> {
        Gpio::<S>::write(self.pin.pin_number, GpioState::High)
    }
    pub fn clear(&mut self) -> Result<(), ErrorCode> {
        Gpio::<S>::write(self.pin.pin_number, GpioState::Low)
    }
}

pub struct InputPin<'a, S: Syscalls, P: Pull> {
    pin: &'a Pin<S>,
    _pull: PhantomData<P>,
}

impl<S: Syscalls, P: Pull> InputPin<'_, S, P> {
    pub fn read(&self) -> Result<GpioState, ErrorCode> {
        Gpio::<S>::read(self.pin.pin_number)
    }

    pub fn enable_interrupts(&self, edge: PinInterruptEdge) -> Result<(), ErrorCode> {
        Gpio::<S>::enable_interrupts(self.pin.pin_number, edge)
    }

    pub fn disable_interrupts(&self) -> Result<(), ErrorCode> {
        Gpio::<S>::disable_interrupts(self.pin.pin_number)
    }
}

impl<S: Syscalls> Drop for OutputPin<'_, S> {
    fn drop(&mut self) {
        let _ = Gpio::<S>::disable(self.pin.pin_number);
    }
}

impl<S: Syscalls, P: Pull> Drop for InputPin<'_, S, P> {
    fn drop(&mut self) {
        let _ = Gpio::<S>::disable(self.pin.pin_number);
    }
}

// -----------------------------------------------------------------------------
// Implementation details below
// -----------------------------------------------------------------------------

impl<S: Syscalls> Gpio<S> {
    fn enable_gpio_output(pin: u32) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_ENABLE_OUTPUT, pin, 0).to_result()
    }

    fn enable_gpio_input(pin: u32, mode: u32) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_ENABLE_INPUT, pin, mode).to_result()
    }

    fn write(pin: u32, state: GpioState) -> Result<(), ErrorCode> {
        let action = match state {
            GpioState::Low => GPIO_CLEAR,
            _ => GPIO_SET,
        };
        S::command(DRIVER_NUM, action, pin, 0).to_result()
    }

    fn read(pin: u32) -> Result<GpioState, ErrorCode> {
        let pin_state: u32 = S::command(DRIVER_NUM, GPIO_READ_INPUT, pin, 0).to_result()?;
        Ok(pin_state.into())
    }

    fn toggle(pin: u32) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_TOGGLE, pin, 0).to_result()
    }

    fn disable(pin: u32) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_DISABLE, pin, 0).to_result()
    }

    fn enable_interrupts(pin: u32, edge: PinInterruptEdge) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_ENABLE_INTERRUPTS, pin, edge as u32).to_result()
    }

    fn disable_interrupts(pin: u32) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_DISABLE_INTERRUPTS, pin, 0).to_result()
    }
}

#[cfg(feature = "rust_embedded")]
impl<S: Syscalls> embedded_hal::digital::ErrorType for OutputPin<'_, S> {
    type Error = ErrorCode;
}

#[cfg(feature = "rust_embedded")]
impl<S: Syscalls> embedded_hal::digital::OutputPin for OutputPin<'_, S> {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.clear()
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.set()
    }
}

// -----------------------------------------------------------------------------
// Async interface
// -----------------------------------------------------------------------------

/// The GPIO-specific half of an [`Edge`].
#[cfg(feature = "async")]
pub struct EdgeOp {
    pin: u32,
    edge: u32,
}

#[cfg(feature = "async")]
impl EdgeOp {
    fn disable_interrupts<S: Syscalls>(pin: u32) {
        let _ = S::command(DRIVER_NUM, GPIO_DISABLE_INTERRUPTS, pin, 0);
    }
}

#[cfg(feature = "async")]
impl<S: Syscalls> libtock_platform::async_call::Operation<S> for EdgeOp {
    /// Which pin fired, and the level it moved to.
    type Value = (u32, GpioState);

    fn start(&self) -> Result<(), ErrorCode> {
        S::command(DRIVER_NUM, GPIO_ENABLE_INTERRUPTS, self.pin, self.edge).to_result()
    }

    fn cancel(&self) {
        Self::disable_interrupts::<S>(self.pin);
    }

    fn complete(&self, args: (u32, u32, u32)) -> Result<Self::Value, ErrorCode> {
        // Disabling here as well as in `cancel` keeps the enable exactly as long
        // as the future, whichever way the future ends.
        Self::disable_interrupts::<S>(self.pin);
        Ok((args.0, args.1.into()))
    }
}

/// A future that resolves on the next GPIO edge.
///
/// This is the first future here over an event nobody requested, and it is where
/// the shape of `Operation` starts to show. Three things to know before using
/// it.
///
/// **It reports which pin fired rather than filtering on it.** Driver 4 has one
/// upcall slot for the whole process, shared by every pin, and the capsule
/// broadcasts: `fired` walks every process holding a grant and schedules the
/// upcall on all of them, whatever pins each one asked about. Since
/// `Operation::complete` has no "that upcall was not mine, keep waiting" answer,
/// an edge on any enabled pin resolves this future, and the pin number comes
/// back in the value. Check it whenever more than one pin can fire.
///
/// Two `Edge` futures alive at once is a bug for the same reason two `Sleep`s
/// are: the second registration replaces the first, and the first to drop
/// unsubscribes for both. One at a time.
///
/// **Edges outside an await are lost.** The subscription lives exactly as long
/// as the future: `Call` registers on first poll and unsubscribes on drop, and
/// the kernel drops upcalls for an unsubscribed slot. A press between two awaits
/// did not happen as far as the process is concerned. Alarm and console have no
/// equivalent gap, because there the kernel only upcalls in answer to a request
/// the process made. This is a sampled edge, not a stream — a `loop` over it
/// misses whatever arrives while the body runs.
///
/// **It owns the pin's interrupt enable** for its whole life, so do not also
/// call [`InputPin::enable_interrupts`] on the same pin. Keep the `InputPin`
/// alive at least as long as the future: dropping it disables the pin outright.
/// The enable is raw hardware state — driver 4 keeps a `Grant<()>`, so unlike
/// the button capsule it holds no per-process record of who wanted an
/// interrupt, and one process disabling a pin ends everyone's edges on it. That
/// is in keeping with driver 4 being the deliberately low-level one; a board
/// that wants ownership should wire the pin to a capsule that models it.
#[cfg(feature = "async")]
pub type Edge<S, C = DefaultConfig> =
    libtock_platform::async_call::Call<S, C, EdgeOp, DRIVER_NUM, INTERRUPT_UPCALL>;

#[cfg(feature = "async")]
impl<S: Syscalls, P: Pull> InputPin<'_, S, P> {
    /// Returns a future that resolves on this pin's next `edge`.
    ///
    /// See [`Edge`] for what the future does and does not promise; it is less
    /// than a caller coming from `Alarm::sleep_for_async` will expect.
    pub fn next_edge(&self, edge: PinInterruptEdge) -> Edge<S> {
        libtock_platform::async_call::Call::new(EdgeOp {
            pin: self.pin.pin_number,
            edge: edge as u32,
        })
    }
}

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------------
// Driver number and command IDs
// -----------------------------------------------------------------------------

const DRIVER_NUM: u32 = 0x4;

// Command IDs
const EXISTS: u32 = 0;

const GPIO_ENABLE_OUTPUT: u32 = 1;
const GPIO_SET: u32 = 2;
const GPIO_CLEAR: u32 = 3;
const GPIO_TOGGLE: u32 = 4;

const GPIO_ENABLE_INPUT: u32 = 5;
const GPIO_READ_INPUT: u32 = 6;

const GPIO_ENABLE_INTERRUPTS: u32 = 7;
const GPIO_DISABLE_INTERRUPTS: u32 = 8;

const GPIO_DISABLE: u32 = 9;

const GPIO_COUNT: u32 = 10;

// Subscribe IDs
const INTERRUPT_UPCALL: u32 = 0;
