//! Blink the two user LEDs on a Pico Breadboard Kit.
//!
//! The kit's LEDs are on GP16 and GP17, driven through the GPIO driver. The
//! `Leds` API is useless on this board: Tock's raspberry_pi_pico_2 points it at
//! GPIO 25, which on a Pico 2 W is the CYW43 radio's chip select, not an LED.

#![no_main]
#![no_std]

use libtock::alarm::{Alarm, Milliseconds};
use libtock::gpio::Gpio;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x400}

const LED_A: u32 = 16;
const LED_B: u32 = 17;

fn main() {
    let mut pin_a = Gpio::get_pin(LED_A).unwrap();
    let mut pin_b = Gpio::get_pin(LED_B).unwrap();
    let mut a = pin_a.make_output().unwrap();
    let mut b = pin_b.make_output().unwrap();

    // Antiphase, so the alternation is unmistakable rather than ambiguous.
    let _ = a.set();
    let _ = b.clear();

    loop {
        let _ = a.toggle();
        let _ = b.toggle();
        Alarm::sleep_for(Milliseconds(500)).unwrap();
    }
}
