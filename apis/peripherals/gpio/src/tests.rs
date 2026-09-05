use core::cell::Cell;

use libtock_platform::{share, ErrorCode, Syscalls, YieldNoWaitReturn};
use libtock_unittest::fake::{self, GpioMode, InterruptEdge, PullMode};

use crate::{GpioInterruptListener, GpioState, PinInterruptEdge, PullDown, PullNone, PullUp};

type Gpio = super::Gpio<fake::Syscalls>;

#[test]
fn no_driver() {
    let _kernel = fake::Kernel::new();
    assert_eq!(Gpio::count(), Err(ErrorCode::NoDevice));
}

#[test]
fn num_gpio() {
    let kernel = fake::Kernel::new();
    let driver = fake::Gpio::<10>::new();
    kernel.add_driver(&driver);
    assert_eq!(Gpio::count(), Ok(10));
}

// Tests the OutputPin implementation.
#[test]
fn output() {
    let kernel = fake::Kernel::new();
    let driver = fake::Gpio::<10>::new();
    driver.set_missing_gpio(1);
    kernel.add_driver(&driver);

    assert_eq!(Gpio::count(), Ok(10));

    assert!(core::matches!(Gpio::get_pin(11), Err(ErrorCode::Invalid)));
    assert!(core::matches!(Gpio::get_pin(1), Err(ErrorCode::NoDevice)));

    let pin_0 = Gpio::get_pin(0);
    assert!(pin_0.is_ok());

    let _ = pin_0.map(|mut pin| {
        let output_pin = pin.make_output();
        assert!(output_pin.is_ok());
        assert_eq!(driver.get_gpio_state(0).unwrap().mode, GpioMode::Output);
        let _ = output_pin.map(|mut pin| {
            assert_eq!(pin.set(), Ok(()));
            assert!(driver.get_gpio_state(0).unwrap().value);
            assert_eq!(pin.clear(), Ok(()));
            assert!(!driver.get_gpio_state(0).unwrap().value);
            assert_eq!(pin.toggle(), Ok(()));
            assert!(driver.get_gpio_state(0).unwrap().value);
            assert_eq!(pin.toggle(), Ok(()));
            assert!(!driver.get_gpio_state(0).unwrap().value);
        });
        assert_eq!(driver.get_gpio_state(0).unwrap().mode, GpioMode::Disable);
    });
}

// Tests the InputPin implementation
#[test]
fn input() {
    let kernel = fake::Kernel::new();
    let driver = fake::Gpio::<10>::new();
    driver.set_missing_gpio(1);
    kernel.add_driver(&driver);

    assert_eq!(Gpio::count(), Ok(10));

    assert!(core::matches!(Gpio::get_pin(11), Err(ErrorCode::Invalid)));
    assert!(core::matches!(Gpio::get_pin(1), Err(ErrorCode::NoDevice)));

    let pin_0 = Gpio::get_pin(0);
    assert!(pin_0.is_ok());

    let _ = pin_0.map(|pin| {
        let input_pin = pin.make_input::<PullNone>();
        assert!(input_pin.is_ok());
        assert_eq!(
            driver.get_gpio_state(0).unwrap().mode,
            GpioMode::Input(PullMode::PullNone)
        );

        let input_pin = pin.make_input::<PullUp>();
        assert!(input_pin.is_ok());
        assert_eq!(
            driver.get_gpio_state(0).unwrap().mode,
            GpioMode::Input(PullMode::PullUp)
        );

        let input_pin = pin.make_input::<PullDown>();
        assert!(input_pin.is_ok());
        assert_eq!(
            driver.get_gpio_state(0).unwrap().mode,
            GpioMode::Input(PullMode::PullDown)
        );

        let _ = input_pin.map(|pin| {
            assert_eq!(driver.set_value(0, true), Ok(()));
            assert_eq!(pin.read(), Ok(GpioState::High));
            assert_eq!(driver.set_value(0, false), Ok(()));
            assert_eq!(pin.read(), Ok(GpioState::Low));
        });
        assert_eq!(driver.get_gpio_state(0).unwrap().mode, GpioMode::Disable);
    });
}

// Tests the pin interrupts implementation
#[test]
fn interrupts() {
    let kernel = fake::Kernel::new();
    let driver = fake::Gpio::<10>::new();
    driver.set_missing_gpio(1);
    kernel.add_driver(&driver);

    assert_eq!(Gpio::count(), Ok(10));

    let gpio_state = Cell::<Option<GpioState>>::new(None);
    let listener = GpioInterruptListener(|gpio, state| {
        assert_eq!(gpio, 0);
        gpio_state.set(Some(state));
    });

    assert_eq!(Gpio::enable_interrupts(0, PinInterruptEdge::Either), Ok(()));
    share::scope(|subscribe| {
        assert_eq!(Gpio::register_listener(&listener, subscribe), Ok(()));
        assert_eq!(driver.set_value(0, true), Ok(()));
        assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
        assert_eq!(gpio_state.get(), Some(GpioState::High));
    });

    assert_eq!(driver.set_value(0, false), Ok(()));
    assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);

    assert!(core::matches!(Gpio::get_pin(11), Err(ErrorCode::Invalid)));
    assert!(core::matches!(Gpio::get_pin(1), Err(ErrorCode::NoDevice)));

    let pin_0 = Gpio::get_pin(0);
    assert!(pin_0.is_ok());

    let _ = pin_0.map(|pin| {
        // Either
        let input_pin = pin.make_input::<PullNone>();
        assert!(input_pin.is_ok());
        assert_eq!(
            driver.get_gpio_state(0).unwrap().mode,
            GpioMode::Input(PullMode::PullNone)
        );

        let _ = input_pin.map(|pin| {
            assert_eq!(
                pin.enable_interrupts(crate::PinInterruptEdge::Either),
                Ok(())
            );
            assert_eq!(
                driver.get_gpio_state(0).unwrap().interrupt_enabled,
                Some(InterruptEdge::Either)
            );

            assert_eq!(driver.set_value(0, false), Ok(()));

            let gpio_state = Cell::<Option<GpioState>>::new(None);
            let listener = GpioInterruptListener(|gpio, state| {
                assert_eq!(gpio, 0);
                gpio_state.set(Some(state));
            });

            share::scope(|subscribe| {
                assert_eq!(Gpio::register_listener(&listener, subscribe), Ok(()));
                assert_eq!(driver.set_value(0, true), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
                assert_eq!(gpio_state.get(), Some(GpioState::High));
                gpio_state.set(None);
                assert_eq!(driver.set_value(0, false), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
                assert_eq!(gpio_state.get(), Some(GpioState::Low));

                assert_eq!(pin.disable_interrupts(), Ok(()));
                assert_eq!(driver.set_value(0, true), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
                assert_eq!(driver.set_value(0, false), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
            });
        });

        // Rising
        let input_pin = pin.make_input::<PullNone>();
        assert!(input_pin.is_ok());
        assert_eq!(
            driver.get_gpio_state(0).unwrap().mode,
            GpioMode::Input(PullMode::PullNone)
        );

        let _ = input_pin.map(|pin| {
            assert_eq!(
                pin.enable_interrupts(crate::PinInterruptEdge::Rising),
                Ok(())
            );
            assert_eq!(
                driver.get_gpio_state(0).unwrap().interrupt_enabled,
                Some(InterruptEdge::Rising)
            );

            assert_eq!(driver.set_value(0, false), Ok(()));

            let gpio_state = Cell::<Option<GpioState>>::new(None);
            let listener = GpioInterruptListener(|gpio, state| {
                assert_eq!(gpio, 0);
                gpio_state.set(Some(state));
            });

            share::scope(|subscribe| {
                assert_eq!(Gpio::register_listener(&listener, subscribe), Ok(()));
                assert_eq!(driver.set_value(0, true), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
                assert_eq!(gpio_state.get(), Some(GpioState::High));
                assert_eq!(driver.set_value(0, false), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);

                assert_eq!(pin.disable_interrupts(), Ok(()));
                assert_eq!(driver.set_value(0, true), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
                assert_eq!(driver.set_value(0, false), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
            });
        });

        // Falling
        let input_pin = pin.make_input::<PullNone>();
        assert!(input_pin.is_ok());
        assert_eq!(
            driver.get_gpio_state(0).unwrap().mode,
            GpioMode::Input(PullMode::PullNone)
        );

        let _ = input_pin.map(|pin| {
            assert_eq!(
                pin.enable_interrupts(crate::PinInterruptEdge::Falling),
                Ok(())
            );
            assert_eq!(
                driver.get_gpio_state(0).unwrap().interrupt_enabled,
                Some(InterruptEdge::Falling)
            );

            assert_eq!(driver.set_value(0, false), Ok(()));

            let gpio_state = Cell::<Option<GpioState>>::new(None);
            let listener = GpioInterruptListener(|gpio, state| {
                assert_eq!(gpio, 0);
                gpio_state.set(Some(state));
            });

            share::scope(|subscribe| {
                assert_eq!(Gpio::register_listener(&listener, subscribe), Ok(()));
                assert_eq!(driver.set_value(0, true), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
                assert_eq!(driver.set_value(0, false), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
                assert_eq!(gpio_state.get(), Some(GpioState::Low));

                assert_eq!(pin.disable_interrupts(), Ok(()));
                assert_eq!(driver.set_value(0, true), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
                assert_eq!(driver.set_value(0, false), Ok(()));
                assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
            });
        });
    });
}

// Tests the pin event subcribe implementation
#[test]
fn subscribe() {
    let kernel = fake::Kernel::new();
    let driver = fake::Gpio::<10>::new();
    driver.set_missing_gpio(1);
    kernel.add_driver(&driver);

    assert_eq!(Gpio::count(), Ok(10));

    let gpio_state = Cell::<Option<GpioState>>::new(None);
    let listener = GpioInterruptListener(|gpio, state| {
        assert_eq!(gpio, 0);
        gpio_state.set(Some(state));
    });

    assert_eq!(Gpio::enable_interrupts(0, PinInterruptEdge::Either), Ok(()));
    share::scope(|subscribe| {
        assert_eq!(Gpio::register_listener(&listener, subscribe), Ok(()));
        assert_eq!(driver.set_value(0, true), Ok(()));
        assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
        assert_eq!(gpio_state.get(), Some(GpioState::High));
    });

    assert_eq!(driver.set_value(0, false), Ok(()));
    assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
}

// -----------------------------------------------------------------------------
// Async interface
// -----------------------------------------------------------------------------

/// Tests for [`Edge`](crate::Edge).
///
/// These exist mostly to pin down what the future does *not* promise. A `Sleep`
/// or a `Read` answers a request the process made, so its upcall can only be
/// the answer; an edge is somebody pressing something, and the difference shows
/// up in three places that are easy to be surprised by.
#[cfg(feature = "async")]
mod edge {
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use super::*;

    /// Walks `pin` high then low. With `Falling` enabled the fake schedules an
    /// upcall for the second transition only, so this delivers exactly one edge.
    fn falling_edge(driver: &fake::Gpio<10>, pin: u32) {
        driver.set_value(pin, true).expect("set_value failed");
        driver.set_value(pin, false).expect("set_value failed");
    }

    /// The happy path, and the enable's lifetime: the future turns the pin's
    /// interrupt on when first polled and off again when it resolves.
    #[test]
    fn edge_resolves_and_gives_the_enable_back() {
        let kernel = fake::Kernel::new();
        let driver = fake::Gpio::<10>::new();
        kernel.add_driver(&driver);
        let pin = Gpio::get_pin(3).expect("pin 3 missing");
        let input = pin.make_input::<PullUp>().expect("make_input failed");

        let mut edge = pin!(input.next_edge(PinInterruptEdge::Falling));
        let mut context = Context::from_waker(Waker::noop());

        assert_eq!(edge.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(
            driver.get_gpio_state(3).unwrap().interrupt_enabled,
            Some(InterruptEdge::Falling),
            "the first poll should have enabled the interrupt"
        );

        falling_edge(&driver, 3);
        fake::Syscalls::yield_wait();

        assert_eq!(
            edge.as_mut().poll(&mut context),
            Poll::Ready(Ok((3, GpioState::Low)))
        );
        assert_eq!(
            driver.get_gpio_state(3).unwrap().interrupt_enabled,
            None,
            "a resolved Edge should leave the interrupt as it found it"
        );
    }

    /// The finding. Driver 4 has one upcall slot per process and the capsule
    /// broadcasts every edge into it, while `Operation::complete` has no way to
    /// say "not mine, keep waiting". So a future awaiting pin 3 resolves on an
    /// edge from pin 5, and the only defence a caller has is the pin number in
    /// the value.
    #[test]
    fn an_edge_on_another_pin_resolves_the_future() {
        let kernel = fake::Kernel::new();
        let driver = fake::Gpio::<10>::new();
        kernel.add_driver(&driver);
        let awaited = Gpio::get_pin(3).expect("pin 3 missing");
        let awaited = awaited.make_input::<PullUp>().expect("make_input failed");
        let other = Gpio::get_pin(5).expect("pin 5 missing");
        let other = other.make_input::<PullUp>().expect("make_input failed");

        other
            .enable_interrupts(PinInterruptEdge::Falling)
            .expect("enable_interrupts failed");

        let mut edge = pin!(awaited.next_edge(PinInterruptEdge::Falling));
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(edge.as_mut().poll(&mut context), Poll::Pending);

        falling_edge(&driver, 5);
        fake::Syscalls::yield_wait();

        assert_eq!(
            edge.as_mut().poll(&mut context),
            Poll::Ready(Ok((5, GpioState::Low))),
            "an edge from a pin this future never asked about still resolves it"
        );
    }

    /// The second finding: this is a sampled edge, not a stream. The
    /// subscription lives exactly as long as the future, and the kernel drops
    /// upcalls for an unsubscribed slot, so a press between two awaits did not
    /// happen as far as the process is concerned.
    #[test]
    fn an_edge_before_the_await_is_lost() {
        let kernel = fake::Kernel::new();
        let driver = fake::Gpio::<10>::new();
        kernel.add_driver(&driver);
        let pin = Gpio::get_pin(3).expect("pin 3 missing");
        let input = pin.make_input::<PullUp>().expect("make_input failed");

        // Enabled outside any future, so the edge is real at the capsule and
        // the only thing missing is somewhere to deliver it.
        input
            .enable_interrupts(PinInterruptEdge::Falling)
            .expect("enable_interrupts failed");
        falling_edge(&driver, 3);

        let mut edge = pin!(input.next_edge(PinInterruptEdge::Falling));
        let mut context = Context::from_waker(Waker::noop());

        assert_eq!(edge.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(
            fake::Syscalls::yield_no_wait(),
            YieldNoWaitReturn::NoUpcall,
            "the edge should have been dropped, not queued"
        );
        assert_eq!(edge.as_mut().poll(&mut context), Poll::Pending);
    }

    /// The cancellation path, which is what makes an `Edge` safe to lose a
    /// `select`: dropping it must hand the pin back rather than leave the
    /// interrupt on with nowhere to deliver it.
    #[test]
    fn dropping_an_edge_disables_the_interrupt() {
        let kernel = fake::Kernel::new();
        let driver = fake::Gpio::<10>::new();
        kernel.add_driver(&driver);
        let pin = Gpio::get_pin(3).expect("pin 3 missing");
        let input = pin.make_input::<PullUp>().expect("make_input failed");

        {
            let mut edge = pin!(input.next_edge(PinInterruptEdge::Falling));
            let mut context = Context::from_waker(Waker::noop());
            assert_eq!(edge.as_mut().poll(&mut context), Poll::Pending);
            assert_eq!(
                driver.get_gpio_state(3).unwrap().interrupt_enabled,
                Some(InterruptEdge::Falling)
            );
        }

        assert_eq!(
            driver.get_gpio_state(3).unwrap().interrupt_enabled,
            None,
            "dropping an Edge must disable the interrupt it enabled"
        );
    }
}
