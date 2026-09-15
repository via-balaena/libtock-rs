//! Fake implementation of the Console API, documented here:
//! https://github.com/tock/tock/blob/master/doc/syscalls/00001_console.md
//!
//! Like the real API, `Console` stores each message written to it.
//! The resulting byte stream can be retrieved via `take_bytes`
//! for use in unit tests.

use core::cell::{Cell, RefCell};
use core::cmp;
use libtock_platform::{CommandReturn, ErrorCode};

use crate::{DriverInfo, DriverShareRef, RoAllowBuffer, RwAllowBuffer};

pub struct Console {
    messages: Cell<Vec<u8>>,
    buffer: Cell<RoAllowBuffer>,

    read_buffer: RefCell<RwAllowBuffer>,
    /// To be returned on read
    input: Cell<Vec<u8>>,
    /// How many bytes an outstanding read asked for, if the driver is
    /// mid-receive. Cleared when the read completes or is aborted.
    receiving: Cell<Option<usize>>,
    /// When set, READ records the request instead of satisfying it at once.
    deferred: bool,

    share_ref: DriverShareRef,
}

impl Console {
    pub fn new() -> std::rc::Rc<Console> {
        Self::new_with_input(b"")
    }

    pub fn new_with_input(inputs: &[u8]) -> std::rc::Rc<Console> {
        Self::build(inputs, false)
    }

    /// A console whose reads stay outstanding until `fire_read` supplies input,
    /// so that `is_receiving` reports something meaningful and a test can check
    /// that aborting a read actually ends it.
    ///
    /// The default constructors answer a read immediately even when there is no
    /// input, reporting zero bytes. That is convenient but unlike the kernel,
    /// where a read with nothing to satisfy it simply stays in flight.
    pub fn new_deferred() -> std::rc::Rc<Console> {
        Self::build(b"", true)
    }

    fn build(inputs: &[u8], deferred: bool) -> std::rc::Rc<Console> {
        std::rc::Rc::new(Console {
            messages: Default::default(),
            buffer: Default::default(),
            read_buffer: Default::default(),
            input: Cell::new(Vec::from(inputs)),
            receiving: Cell::new(None),
            deferred,
            share_ref: Default::default(),
        })
    }

    /// Whether a read is in flight. Always false for a console built with
    /// `new` or `new_with_input`, which answer a read before READ returns.
    pub fn is_receiving(&self) -> bool {
        self.receiving.get().is_some()
    }

    /// Supplies input and completes an outstanding read. Does nothing when no
    /// read is in flight.
    pub fn fire_read(&self, input: &[u8]) {
        let mut pending = self.input.take();
        pending.extend_from_slice(input);
        self.input.set(pending);
        self.complete_read();
    }

    /// Satisfies an outstanding read from whatever input is buffered.
    fn complete_read(&self) {
        let count_wanted = match self.receiving.take() {
            Some(count) => count,
            None => return,
        };

        let bytes = self.input.take();
        // Clamp to the allowed buffer as well as the request: a caller driving
        // this driver directly may not have allowed a buffer at all, and the
        // kernel would never write past the one it was given.
        let count_wanted = cmp::min(count_wanted, self.read_buffer.borrow().len());
        let count_wanted = cmp::min(count_wanted, bytes.len());
        let to_send = &bytes[..count_wanted];
        let to_keep = &bytes[count_wanted..];
        self.input.set(Vec::from(to_keep));

        let count_available = to_send.len();
        self.read_buffer.borrow_mut()[..count_wanted].copy_from_slice(to_send);
        self.share_ref
            .schedule_upcall(SUBSCRIBE_READ, (0, count_available as u32, 0))
            .expect("Unable to schedule upcall {}");
    }

    /// Returns the bytes that have been submitted so far,
    /// and clears them.
    pub fn take_bytes(&self) -> Vec<u8> {
        self.messages.take()
    }
}

impl crate::fake::SyscallDriver for Console {
    fn info(&self) -> DriverInfo {
        DriverInfo::new(DRIVER_NUM).upcall_count(3)
    }

    fn register(&self, share_ref: DriverShareRef) {
        self.share_ref.replace(share_ref);
    }

    fn allow_readonly(
        &self,
        buffer_num: u32,
        buffer: RoAllowBuffer,
    ) -> Result<RoAllowBuffer, (RoAllowBuffer, ErrorCode)> {
        if buffer_num == ALLOW_WRITE {
            Ok(self.buffer.replace(buffer))
        } else {
            Err((buffer, ErrorCode::Invalid))
        }
    }

    fn allow_readwrite(
        &self,
        buffer_num: u32,
        buffer: RwAllowBuffer,
    ) -> Result<RwAllowBuffer, (RwAllowBuffer, ErrorCode)> {
        if buffer_num == ALLOW_READ {
            Ok(self.read_buffer.replace(buffer))
        } else {
            Err((buffer, ErrorCode::Invalid))
        }
    }

    fn command(&self, command_num: u32, argument0: u32, _argument1: u32) -> CommandReturn {
        match command_num {
            EXISTS => {}
            WRITE => {
                let mut bytes = self.messages.take();
                let buffer = self.buffer.take();
                let size = cmp::min(buffer.len(), argument0 as usize);
                bytes.extend_from_slice(&(*buffer)[..size]);
                self.buffer.set(buffer);
                self.messages.set(bytes);
                self.share_ref
                    .schedule_upcall(SUBSCRIBE_WRITE, (size as u32, 0, 0))
                    .expect("Unable to schedule upcall {}");
            }
            READ => {
                self.receiving.set(Some(argument0 as usize));
                if !self.deferred {
                    self.complete_read();
                }
            }
            ABORT => {
                // Mirrors capsules/core/src/console.rs: abort cancels an
                // in-progress receive and reports what arrived so far through
                // the upcall. It succeeds either way, so nothing outstanding is
                // not an error.
                if self.receiving.take().is_some() {
                    self.share_ref
                        .schedule_upcall(SUBSCRIBE_READ, (0, 0, 0))
                        .expect("Unable to schedule upcall {}");
                }
            }
            _ => return crate::command_return::failure(ErrorCode::NoSupport),
        }
        crate::command_return::success()
    }
}

// -----------------------------------------------------------------------------
// Implementation details below
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests;

const DRIVER_NUM: u32 = 0x1;

// Command numbers
const EXISTS: u32 = 0;
const WRITE: u32 = 1;
const READ: u32 = 2;
const ABORT: u32 = 3;
const SUBSCRIBE_WRITE: u32 = 1;
const SUBSCRIBE_READ: u32 = 2;
const ALLOW_WRITE: u32 = 1;
const ALLOW_READ: u32 = 1;
