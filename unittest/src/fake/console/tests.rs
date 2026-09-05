use crate::fake;
use crate::{RoAllowBuffer, RwAllowBuffer};
use libtock_platform::share;
use libtock_platform::DefaultConfig;

// Tests the command implementation.
#[test]
fn command() {
    use fake::SyscallDriver;
    let console = fake::Console::new();
    assert!(console.command(fake::console::EXISTS, 1, 2).is_success());
    assert!(console.allow_readonly(1, RoAllowBuffer::default()).is_ok());
    assert!(console.allow_readonly(2, RoAllowBuffer::default()).is_err());

    assert!(console.allow_readwrite(1, RwAllowBuffer::default()).is_ok());
    assert!(console
        .allow_readwrite(2, RwAllowBuffer::default())
        .is_err());
}

// Integration test that verifies Console works with fake::Kernel and
// libtock_platform::Syscalls.
#[test]
fn kernel_integration() {
    use libtock_platform::Syscalls;
    let kernel = fake::Kernel::new();
    let console = fake::Console::new();
    kernel.add_driver(&console);
    assert!(
        fake::Syscalls::command(fake::console::DRIVER_NUM, fake::console::EXISTS, 1, 2)
            .is_success()
    );
    share::scope(|allow_ro| {
        fake::Syscalls::allow_ro::<
            DefaultConfig,
            { fake::console::DRIVER_NUM },
            { fake::console::ALLOW_WRITE },
        >(allow_ro, b"abcd")
        .unwrap();
        assert!(
            fake::Syscalls::command(fake::console::DRIVER_NUM, fake::console::WRITE, 3, 0)
                .is_success()
        );
    });
    assert_eq!(console.take_bytes(), b"abc");
    assert_eq!(console.take_bytes(), b"");

    let mut buf = [0; 4];

    share::scope(|allow_rw| {
        fake::Syscalls::allow_rw::<
            DefaultConfig,
            { fake::console::DRIVER_NUM },
            { fake::console::ALLOW_READ },
        >(allow_rw, &mut buf)
        .unwrap();
        assert!(
            fake::Syscalls::command(fake::console::DRIVER_NUM, fake::console::READ, 3, 0)
                .is_success()
        );
    });
}

// Aborting an outstanding read must end it. Without this a consumer-side
// cancellation test can only assert that command 3 was sent.
#[test]
fn abort_ends_an_outstanding_read() {
    use fake::SyscallDriver;
    let kernel = fake::Kernel::new();
    let console = fake::Console::new_deferred();
    kernel.add_driver(&console);

    assert!(!console.is_receiving());
    assert!(console.command(fake::console::READ, 8, 0).is_success());
    assert!(console.is_receiving(), "a deferred read stays in flight");

    assert!(console.command(fake::console::ABORT, 0, 0).is_success());
    assert!(!console.is_receiving(), "abort must end the receive");

    // The kernel's abort succeeds whether or not anything was outstanding.
    assert!(console.command(fake::console::ABORT, 0, 0).is_success());
}

// The default constructors answer a read before READ returns, even with no
// input to give. Blocking `Console::read` relies on that, so pin it.
#[test]
fn new_answers_a_read_immediately() {
    use fake::SyscallDriver;
    let kernel = fake::Kernel::new();
    let console = fake::Console::new();
    kernel.add_driver(&console);

    assert!(console.command(fake::console::READ, 8, 0).is_success());
    assert!(!console.is_receiving());
}

// A deferred read ends when input is supplied. The bytes themselves are not
// checked here -- no buffer is allowed when driving the driver directly -- and
// libtock_async's console_read_resolves covers delivery.
#[test]
fn deferred_read_completes_on_input() {
    use fake::SyscallDriver;
    let kernel = fake::Kernel::new();
    let console = fake::Console::new_deferred();
    kernel.add_driver(&console);

    assert!(console.command(fake::console::READ, 8, 0).is_success());
    assert!(console.is_receiving());

    console.fire_read(b"hi");
    assert!(
        !console.is_receiving(),
        "supplying input completes the read"
    );
}
