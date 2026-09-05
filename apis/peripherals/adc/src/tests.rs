use core::cell::Cell;
use libtock_platform::{share, ErrorCode, Syscalls, YieldNoWaitReturn};
use libtock_unittest::fake;

type Adc = super::Adc<fake::Syscalls>;

#[test]
fn no_driver() {
    let _kernel = fake::Kernel::new();
    assert_eq!(Adc::exists(), Err(ErrorCode::NoDevice));
}

#[test]
fn exists() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new();
    kernel.add_driver(&driver);

    assert_eq!(Adc::exists(), Ok(()));
}

#[test]
fn read_single_sample() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new();
    kernel.add_driver(&driver);

    assert_eq!(Adc::read_single_sample(0), Ok(()));
    assert!(driver.is_busy());

    assert_eq!(Adc::read_single_sample(0), Err(ErrorCode::Busy));
    assert_eq!(Adc::read_single_sample_sync(0), Err(ErrorCode::Busy));
}

#[test]
fn register_unregister_listener() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new();
    kernel.add_driver(&driver);

    let sample: Cell<Option<u16>> = Cell::new(None);
    let listener = crate::ADCListener(|adc_val| {
        sample.set(Some(adc_val));
    });
    share::scope(|subscribe| {
        assert_eq!(Adc::read_single_sample(0), Ok(()));
        driver.set_value(100);
        assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);

        assert_eq!(Adc::register_listener(&listener, subscribe), Ok(()));
        assert_eq!(Adc::read_single_sample(0), Ok(()));
        driver.set_value(100);
        assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::Upcall);
        assert_eq!(sample.get(), Some(100));

        Adc::unregister_listener();
        assert_eq!(Adc::read_single_sample(0), Ok(()));
        driver.set_value(100);
        assert_eq!(fake::Syscalls::yield_no_wait(), YieldNoWaitReturn::NoUpcall);
    });
}

#[test]
fn read_single_sample_sync() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new();
    kernel.add_driver(&driver);

    driver.set_value_sync(1000);
    assert_eq!(Adc::read_single_sample_sync(0), Ok(1000));
}

/// The gap this argument exists to close. A board with more than one analogue
/// input was unreachable past channel 0: `read_single_sample` sent a hardcoded
/// zero, and this fake ignored the argument, so nothing on either side could
/// notice. A joystick is two channels, which is how it surfaced.
#[test]
fn a_sample_goes_to_the_channel_it_names() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new_with_channels(3);
    kernel.add_driver(&driver);

    driver.set_value_sync_on(0, 2048);
    driver.set_value_sync_on(1, 512);

    assert_eq!(Adc::read_single_sample_sync(0), Ok(2048));
    assert_eq!(driver.last_channel(), Some(0));

    assert_eq!(Adc::read_single_sample_sync(1), Ok(512));
    assert_eq!(
        driver.last_channel(),
        Some(1),
        "the channel must reach the kernel, not be dropped on the way"
    );
}

/// The capsule checks the channel against the board's list before anything
/// else, so asking for one the board does not have is refused rather than
/// silently read from channel 0.
#[test]
fn a_channel_the_board_lacks_is_refused() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new_with_channels(2);
    kernel.add_driver(&driver);

    assert_eq!(Adc::read_single_sample(2), Err(ErrorCode::NoDevice));
    assert!(!driver.is_busy());
    assert_eq!(driver.last_channel(), None);
}

/// `exists` throws away the number the kernel returns, which is the only
/// portable way to learn how many channels a board has.
#[test]
fn count_reports_the_boards_channels() {
    let kernel = fake::Kernel::new();
    let driver = fake::Adc::new_with_channels(4);
    kernel.add_driver(&driver);

    assert_eq!(Adc::count(), Ok(4));
    assert_eq!(Adc::exists(), Ok(()));
}

#[test]
fn count_without_a_driver_is_an_error() {
    let _kernel = fake::Kernel::new();
    assert_eq!(Adc::count(), Err(ErrorCode::NoDevice));
}
