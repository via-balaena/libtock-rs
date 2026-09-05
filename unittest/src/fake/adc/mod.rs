//! Fake implementation of the Adc API, documented here:
//!
//! Like the real API, `Adc` controls a fake Adc sensor. It provides
//! a function `set_value` used to immediately call an upcall with a Adc value read by the sensor
//! and a function 'set_value_sync' used to call the upcall when the read command is received.

use crate::{DriverInfo, DriverShareRef};
use libtock_platform::{CommandReturn, ErrorCode};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

// The `upcall_on_command` map holds the value an upcall should carry when a read
// command is received, keyed by channel; a channel with no entry schedules no
// upcall. It was needed for testing `read_sync` library function which simulates
// a synchronous Adc read, because it was impossible to schedule an upcall during
// the `synchronous` read in other ways.
pub struct Adc {
    busy: Cell<bool>,
    upcall_on_command: RefCell<HashMap<u32, i32>>,
    /// How many channels this board exposes, as command 0 reports it.
    channels: Cell<u32>,
    /// The channel of the most recent sample request.
    last_channel: Cell<Option<u32>>,
    share_ref: DriverShareRef,
}

impl Adc {
    /// A one-channel ADC, which is what a board with a single analogue input
    /// looks like.
    pub fn new() -> std::rc::Rc<Adc> {
        Self::new_with_channels(1)
    }

    /// An ADC exposing `channels` channels, indexed from zero.
    ///
    /// Worth reaching for whenever the code under test names a channel: with
    /// one channel every channel number a caller could pass is either 0 or out
    /// of range, so a driver that ignored the argument entirely would still
    /// pass. That is not hypothetical -- this fake did ignore it, and the
    /// libtock_adc API hardcoded channel 0 underneath it, undetected.
    pub fn new_with_channels(channels: u32) -> std::rc::Rc<Adc> {
        std::rc::Rc::new(Adc {
            busy: Cell::new(false),
            upcall_on_command: Default::default(),
            channels: Cell::new(channels),
            last_channel: Cell::new(None),
            share_ref: Default::default(),
        })
    }

    pub fn is_busy(&self) -> bool {
        self.busy.get()
    }

    /// The channel of the most recent sample request, or `None` if there has
    /// not been one.
    pub fn last_channel(&self) -> Option<u32> {
        self.last_channel.get()
    }

    pub fn set_value(&self, value: i32) {
        if self.busy.get() {
            self.share_ref
                .schedule_upcall(0, (value as u32, 0, 0))
                .expect("Unable to schedule upcall");
            self.busy.set(false);
        }
    }

    /// Makes a synchronous read of channel 0 return `value`.
    pub fn set_value_sync(&self, value: i32) {
        self.set_value_sync_on(0, value);
    }

    /// Makes a synchronous read of `channel` return `value`.
    pub fn set_value_sync_on(&self, channel: u32, value: i32) {
        self.upcall_on_command.borrow_mut().insert(channel, value);
    }
}

impl crate::fake::SyscallDriver for Adc {
    fn info(&self) -> DriverInfo {
        DriverInfo::new(DRIVER_NUM).upcall_count(1)
    }

    fn register(&self, share_ref: DriverShareRef) {
        self.share_ref.replace(share_ref);
    }

    fn command(&self, command_id: u32, argument0: u32, _argument1: u32) -> CommandReturn {
        match command_id {
            EXISTS => crate::command_return::success_u32(self.channels.get()),

            SINGLE_SAMPLE => {
                // The capsule checks the channel against the board's list
                // before anything else and answers NoDevice for one it does not
                // have -- see enqueue_command in capsules/core/src/adc.rs.
                if argument0 >= self.channels.get() {
                    return crate::command_return::failure(ErrorCode::NoDevice);
                }
                if self.busy.get() {
                    return crate::command_return::failure(ErrorCode::Busy);
                }
                self.last_channel.set(Some(argument0));
                self.busy.set(true);
                if let Some(val) = self.upcall_on_command.borrow_mut().remove(&argument0) {
                    self.set_value(val);
                }
                crate::command_return::success()
            }
            _ => crate::command_return::failure(ErrorCode::NoSupport),
        }
    }
}

#[cfg(test)]
mod tests;
// -----------------------------------------------------------------------------
// Driver number and command IDs
// -----------------------------------------------------------------------------

const DRIVER_NUM: u32 = 0x5;

// Command IDs

const EXISTS: u32 = 0;
const SINGLE_SAMPLE: u32 = 1;
