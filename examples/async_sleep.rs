//! The blink app, written against the async alarm.
//!
//! Functionally identical to `blink.rs`; the difference is that the wait is a
//! `Future` driven by `block_on` rather than a bespoke yield loop inside
//! `sleep_for`. Useful as the smallest thing that exercises the async stack end
//! to end, and as the baseline for what the executor costs in flash.
//!
//! Requires the `async` feature:
//!
//! ```text
//! make microbit_v2 EXAMPLE=async_sleep FEATURES=async
//! ```

#![no_main]
#![no_std]

use libtock::alarm::{Alarm, Milliseconds};
use libtock::futures::block_on;
use libtock::leds::Leds;
use libtock::runtime::{set_main, stack_size, TockSyscalls};

set_main! {main}
stack_size! {0x400}

fn main() {
    if let Ok(leds_count) = Leds::count() {
        block_on::<TockSyscalls, _>(async {
            let mut count = 0;
            loop {
                for led_index in 0..leds_count {
                    if count & (1 << led_index) > 0 {
                        let _ = Leds::on(led_index);
                    } else {
                        let _ = Leds::off(led_index);
                    }
                }

                // The await point is the whole demonstration: the process is
                // parked in `yield_wait` inside `block_on`, and the alarm upcall
                // resumes this state machine rather than a loop in the driver.
                if Alarm::sleep_for_async(Milliseconds(250))
                    .expect("no alarm driver")
                    .await
                    .is_err()
                {
                    return;
                }

                count += 1;
            }
        });
    }
}
