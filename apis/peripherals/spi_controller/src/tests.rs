use libtock_platform::ErrorCode;
use libtock_unittest::fake;

use crate::{ClockPhase, ClockPolarity};

type SpiController = super::SpiController<fake::Syscalls>;

#[test]
fn no_driver() {
    let _kernel = fake::Kernel::new();
    assert_eq!(SpiController::exists(), Err(ErrorCode::NoDevice));
}

#[test]
fn exists() {
    let kernel = fake::Kernel::new();
    let driver = fake::SpiController::new();
    kernel.add_driver(&driver);

    assert_eq!(SpiController::exists(), Ok(()));
}

/// A rate is a request, not an instruction: the kernel sets the closest its
/// divider can make. The fake rounds down to model that, so a caller who
/// assumes the number it asked for is the number it got fails here rather than
/// against a peripheral with a maximum.
#[test]
fn a_baud_rate_is_asked_for_not_set() {
    let kernel = fake::Kernel::new();
    let driver = fake::SpiController::new();
    kernel.add_driver(&driver);
    driver.set_rate_step(1_000_000);

    assert_eq!(SpiController::set_baud_rate(6_600_000), Ok(()));
    assert_eq!(
        SpiController::get_baud_rate(),
        Ok(6_000_000),
        "the achievable rate is what the bus runs at, not the requested one"
    );
}

#[test]
fn phase_and_polarity_read_back_what_was_written() {
    let kernel = fake::Kernel::new();
    let driver = fake::SpiController::new();
    kernel.add_driver(&driver);

    assert_eq!(SpiController::get_phase(), Ok(ClockPhase::SampleLeading));
    assert_eq!(SpiController::get_polarity(), Ok(ClockPolarity::IdleLow));

    assert_eq!(SpiController::set_phase(ClockPhase::SampleTrailing), Ok(()));
    assert_eq!(SpiController::set_polarity(ClockPolarity::IdleHigh), Ok(()));

    assert_eq!(SpiController::get_phase(), Ok(ClockPhase::SampleTrailing));
    assert_eq!(SpiController::get_polarity(), Ok(ClockPolarity::IdleHigh));
}

#[test]
fn a_transfer_carries_both_directions() {
    let kernel = fake::Kernel::new();
    let driver = fake::SpiController::new();
    kernel.add_driver(&driver);

    // What a display controller would shift back after a read-id command.
    driver.set_response(&[0x00, 0x00, 0x94, 0x86]);

    let write = [0xd3u8, 0x00, 0x00, 0x00];
    let mut read = [0u8; 4];
    assert_eq!(
        SpiController::spi_controller_write_read_sync(&write, &mut read, 4),
        Ok(())
    );

    assert_eq!(driver.take_written(), write);
    assert_eq!(read, [0x00, 0x00, 0x94, 0x86]);
}

/// A device that is not there, or one whose data line is not connected, reads
/// as zeros rather than failing. Worth a test because it is the result a probe
/// has to be able to tell apart from a real answer.
#[test]
fn a_silent_device_reads_as_zeros() {
    let kernel = fake::Kernel::new();
    let driver = fake::SpiController::new();
    kernel.add_driver(&driver);

    let mut read = [0xffu8; 4];
    assert_eq!(
        SpiController::spi_controller_read_sync(&mut read, 4),
        Ok(())
    );

    assert_eq!(read, [0, 0, 0, 0]);
    assert_eq!(
        driver.take_written(),
        [0xff, 0xff, 0xff, 0xff],
        "a read clocks 0xff out, which is what the capsule does"
    );
}
