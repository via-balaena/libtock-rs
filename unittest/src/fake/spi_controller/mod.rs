//! Fake implementation of the SPI controller API, documented here:
//! https://github.com/tock/tock/blob/master/doc/syscalls/20001_spi.md
//!
//! Models the bus configuration and the transfer path: what a transfer wrote is
//! recorded, and what it reads back comes from a response set in advance, so a
//! driver that talks to a device can be tested without one.
//!
//! What it does not model is chip select. The capsule does not expose one
//! either — command 3 answers `NOSUPPORT` — so a driver that needs CS held
//! across a transfer boundary drives it as a GPIO, and that belongs to
//! `fake::Gpio`.

use std::cell::{Cell, RefCell};

use libtock_platform::{CommandReturn, ErrorCode};

use crate::{DriverInfo, DriverShareRef, RoAllowBuffer, RwAllowBuffer};

pub struct SpiController {
    rate: Cell<u32>,
    /// The granularity of the rate the fake can produce, modelling a divider.
    /// A request is rounded down to a multiple of this, so a test can show that
    /// asking for a rate and getting it are different things.
    rate_step: Cell<u32>,
    phase: Cell<u32>,
    polarity: Cell<u32>,

    write_buffer: RefCell<RoAllowBuffer>,
    read_buffer: RefCell<RwAllowBuffer>,

    /// What the device shifts back during a transfer. Consumed from the front.
    response: RefCell<Vec<u8>>,
    /// Everything written since the last `take_written`.
    written: RefCell<Vec<u8>>,

    share_ref: DriverShareRef,
}

impl SpiController {
    pub fn new() -> std::rc::Rc<SpiController> {
        std::rc::Rc::new(SpiController {
            rate: Cell::new(1_000_000),
            rate_step: Cell::new(1),
            phase: Cell::new(0),
            polarity: Cell::new(0),
            write_buffer: Default::default(),
            read_buffer: Default::default(),
            response: Default::default(),
            written: Default::default(),
            share_ref: Default::default(),
        })
    }

    /// Makes the fake round a requested rate down to a multiple of `step`, the
    /// way a clock divider does. Set it to model a bus that cannot give a
    /// caller the rate it asked for.
    pub fn set_rate_step(&self, step: u32) {
        self.rate_step.set(step.max(1));
    }

    /// Queues bytes for the device to shift back. A transfer that outruns them
    /// reads zeros, which is what an absent or silent device looks like.
    pub fn set_response(&self, bytes: &[u8]) {
        *self.response.borrow_mut() = bytes.to_vec();
    }

    /// Everything written since this was last called.
    pub fn take_written(&self) -> Vec<u8> {
        std::mem::take(&mut self.written.borrow_mut())
    }

    /// Runs one transfer of `len` bytes: records what went out, fills what
    /// comes back, and completes.
    fn transfer(&self, len: usize, write: bool) {
        if write {
            let buffer = self.write_buffer.borrow();
            let size = len.min(buffer.len());
            self.written.borrow_mut().extend_from_slice(&buffer[..size]);
        } else {
            // Command 11 clocks 0xFF out while it reads.
            self.written
                .borrow_mut()
                .extend(std::iter::repeat_n(0xff, len));
        }

        let mut response = self.response.borrow_mut();
        let mut read = self.read_buffer.borrow_mut();
        for index in 0..len.min(read.len()) {
            read[index] = if response.is_empty() {
                0
            } else {
                response.remove(0)
            };
        }
        drop(read);
        drop(response);

        self.share_ref
            .schedule_upcall(SUBSCRIBE_COMPLETE, (len as u32, 0, 0))
            .expect("Unable to schedule upcall");
    }
}

impl crate::fake::SyscallDriver for SpiController {
    fn info(&self) -> DriverInfo {
        DriverInfo::new(DRIVER_NUM).upcall_count(1)
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
            Ok(self.write_buffer.replace(buffer))
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
            EXISTS => crate::command_return::success(),

            READ_WRITE_BYTES | INPLACE_READ_WRITE_BYTES => {
                self.transfer(argument0 as usize, true);
                crate::command_return::success()
            }
            READ_BYTES => {
                self.transfer(argument0 as usize, false);
                crate::command_return::success()
            }

            SET_BAUD => {
                let step = self.rate_step.get();
                self.rate.set(argument0 / step * step);
                crate::command_return::success()
            }
            GET_BAUD => crate::command_return::success_u32(self.rate.get()),

            SET_PHASE => {
                self.phase.set(u32::from(argument0 != 0));
                crate::command_return::success()
            }
            GET_PHASE => crate::command_return::success_u32(self.phase.get()),

            SET_POLARITY => {
                self.polarity.set(u32::from(argument0 != 0));
                crate::command_return::success()
            }
            GET_POLARITY => crate::command_return::success_u32(self.polarity.get()),

            _ => crate::command_return::failure(ErrorCode::NoSupport),
        }
    }
}

// -----------------------------------------------------------------------------
// Driver number, command IDs, and subscribe/allow numbers
// -----------------------------------------------------------------------------

const DRIVER_NUM: u32 = 0x20001;

const SUBSCRIBE_COMPLETE: u32 = 0;
const ALLOW_WRITE: u32 = 0;
const ALLOW_READ: u32 = 0;

const EXISTS: u32 = 0;
const READ_WRITE_BYTES: u32 = 2;
const SET_BAUD: u32 = 5;
const GET_BAUD: u32 = 6;
const SET_PHASE: u32 = 7;
const GET_PHASE: u32 = 8;
const SET_POLARITY: u32 = 9;
const GET_POLARITY: u32 = 10;
const READ_BYTES: u32 = 11;
const INPLACE_READ_WRITE_BYTES: u32 = 12;
