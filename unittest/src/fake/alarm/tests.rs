use crate::fake;
use fake::alarm::*;

// Tests the command implementation.
#[test]
fn command() {
    use fake::SyscallDriver;
    let alarm = Alarm::new(10);

    assert_eq!(
        alarm.command(command::FREQUENCY, 1, 2).get_success_u32(),
        Some(10)
    );
}

// Tests that stop clears an outstanding alarm, which is what makes a
// cancellation test in a consumer crate mean anything.
#[test]
fn stop_clears_the_outstanding_alarm() {
    use fake::SyscallDriver;
    let kernel = fake::Kernel::new();
    let alarm = Alarm::new_deferred(1000);
    kernel.add_driver(&alarm);

    assert!(!alarm.is_armed(), "nothing set yet");
    assert_eq!(
        alarm.command(command::STOP, 0, 0).get_failure(),
        Some(libtock_platform::ErrorCode::Already),
        "stopping with nothing outstanding reports ALREADY, as the kernel does"
    );

    let _ = alarm.command(command::SET_RELATIVE, 100, 0);
    assert!(alarm.is_armed());
    assert_eq!(alarm.expiration(), Some(100));

    assert!(alarm.command(command::STOP, 0, 0).is_success());
    assert!(!alarm.is_armed(), "stop must clear the deadline");
}

// The default constructor's alarm fires before SET_RELATIVE returns, so nothing
// is ever observably outstanding. That is deliberate -- blocking `sleep_for`
// relies on it -- and worth pinning so it is not changed by accident.
#[test]
fn new_fires_before_set_relative_returns() {
    use fake::SyscallDriver;
    let kernel = fake::Kernel::new();
    let alarm = Alarm::new(1000);
    kernel.add_driver(&alarm);

    let _ = alarm.command(command::SET_RELATIVE, 100, 0);
    assert!(
        !alarm.is_armed(),
        "firing consumes the deadline, so nothing is left outstanding"
    );
}

// A deferred alarm delivers only when told to.
#[test]
fn deferred_fires_on_request() {
    use fake::SyscallDriver;
    let kernel = fake::Kernel::new();
    let alarm = Alarm::new_deferred(1000);
    kernel.add_driver(&alarm);

    let _ = alarm.command(command::SET_RELATIVE, 100, 0);
    assert!(alarm.is_armed(), "deferred means it stays outstanding");

    alarm.fire();
    assert!(!alarm.is_armed(), "firing consumes the deadline");

    // Delivery itself is not asserted here: `schedule_upcall` queues nothing
    // when no process has subscribed, and these tests drive the driver
    // directly. The consumer-side tests in libtock_async cover delivery.
    alarm.fire();
    assert!(
        !alarm.is_armed(),
        "firing again with nothing set is a no-op"
    );
}
