//! Fake implementation of the Alarm API.
//!
//! Supports frequency, set_relative and stop.
//!
//! An alarm built with `new` fires before `SET_RELATIVE` returns, which is what
//! most tests want: a blocking `sleep_for` then terminates without the test
//! having to advance time. One built with `new_deferred` records the deadline
//! instead, so `is_armed` reports something meaningful and a test can check that
//! stopping an alarm actually clears it -- see `fire`.

use core::cell::Cell;
use core::num::Wrapping;
use libtock_platform::{CommandReturn, ErrorCode};

use crate::{DriverInfo, DriverShareRef};

pub struct Alarm {
    frequency_hz: u32,
    now: Cell<Wrapping<u32>>,
    /// The deadline of the outstanding alarm, mirroring the kernel's
    /// `td.expiration`. Cleared when the alarm fires or is stopped.
    expiration: Cell<Option<u32>>,
    /// When set, `SET_RELATIVE` records the deadline without delivering.
    deferred: bool,
    share_ref: DriverShareRef,
}

impl Alarm {
    /// An alarm that fires as soon as it is set.
    pub fn new(frequency_hz: u32) -> std::rc::Rc<Alarm> {
        Self::build(frequency_hz, false)
    }

    /// An alarm that records deadlines without firing them, so that an
    /// outstanding alarm is observable. Call `fire` to deliver the upcall.
    pub fn new_deferred(frequency_hz: u32) -> std::rc::Rc<Alarm> {
        Self::build(frequency_hz, true)
    }

    fn build(frequency_hz: u32, deferred: bool) -> std::rc::Rc<Alarm> {
        std::rc::Rc::new(Alarm {
            frequency_hz,
            now: Cell::new(Wrapping(0)),
            expiration: Cell::new(None),
            deferred,
            share_ref: Default::default(),
        })
    }

    /// The deadline of the outstanding alarm, if one is set.
    pub fn expiration(&self) -> Option<u32> {
        self.expiration.get()
    }

    /// Whether an alarm is outstanding. Always false for a driver built with
    /// `new`, which fires before `SET_RELATIVE` returns.
    pub fn is_armed(&self) -> bool {
        self.expiration.get().is_some()
    }

    /// Delivers the upcall for an outstanding alarm and clears it. Does nothing
    /// when no alarm is set.
    pub fn fire(&self) {
        if let Some(deadline) = self.expiration.take() {
            self.share_ref
                .schedule_upcall(subscribe::CALLBACK, (deadline, 0, 0))
                .expect("schedule_upcall failed");
        }
    }
}

impl crate::fake::SyscallDriver for Alarm {
    fn info(&self) -> DriverInfo {
        DriverInfo::new(DRIVER_NUM).upcall_count(1)
    }

    fn register(&self, share_ref: DriverShareRef) {
        self.share_ref.replace(share_ref);
    }

    fn command(&self, command_number: u32, argument0: u32, _argument1: u32) -> CommandReturn {
        match command_number {
            command::FREQUENCY => crate::command_return::success_u32(self.frequency_hz),
            command::STOP => {
                // Mirrors capsules/core/src/alarm.rs: stopping when nothing is
                // outstanding reports ALREADY rather than succeeding.
                match self.expiration.take() {
                    Some(_) => crate::command_return::success(),
                    None => crate::command_return::failure(ErrorCode::Already),
                }
            }
            command::SET_RELATIVE => {
                // We're not actually sleeping, just ticking the timer.
                // The semantics of sleeping aren't clear,
                // so we're assuming that all future times are equal.
                let relative = argument0;
                let wake = self.now.get() + Wrapping(relative);
                self.now.set(wake);
                self.expiration.set(Some(wake.0));
                if !self.deferred {
                    self.fire();
                }
                crate::command_return::success_u32(wake.0)
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

const DRIVER_NUM: u32 = 0x0;

// Command IDs
#[allow(unused)]
pub mod command {
    pub const EXISTS: u32 = 0;
    pub const FREQUENCY: u32 = 1;
    pub const TIME: u32 = 2;
    pub const STOP: u32 = 3;

    pub const SET_RELATIVE: u32 = 5;
    pub const SET_ABSOLUTE: u32 = 6;
}

#[allow(unused)]
pub mod subscribe {
    pub const CALLBACK: u32 = 0;
}
