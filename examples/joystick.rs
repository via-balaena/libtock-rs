//! Reads the kit's two-axis joystick on GP26 and GP27.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=joystick
//! ```
//!
//! Needs a kernel that exposes the ADC with at least two channels. Which
//! physical pin each channel reaches is a board decision, so this names
//! channels rather than pins and prints the count it found; on the Pico 2 W
//! kit, channel 0 is GP26 and channel 1 is GP27.
//!
//! Nothing here assumes which way the stick is wired. The rest position is
//! measured at startup rather than taken to be mid-scale — a joystick's centre
//! never is — and the extremes are accumulated as you move it, so the output
//! tells you the travel instead of requiring it to be known first. Which
//! direction counts as up is likewise yours to read off the numbers.
//!
//! A note on the scale, because it is a trap. `Adc::get_resolution_bits`
//! reports 12, but the sample is not a 12-bit number: it is the raw value
//! left-justified in a `u16`, so it spans the whole 16-bit range in steps of
//! sixteen. That is a stated contract rather than a chip habit —
//! `kernel/src/hil/adc.rs` carries "All ADC samples will be the raw ADC value
//! left-justified in the u16" on every method that returns one — and rp2xxx,
//! nrf52 and stm32f4xx all shift by four in compliance with it.
//!
//! What does not say so is the side an application reads.
//! `doc/syscalls/00005_adc.md` offers only that the number of bits per sample
//! is chip specific, and the capsule forwards the sample and the resolution
//! through unchanged, so the kernel's guarantee never reaches the app author.
//! Read 12, compute a full scale of `(1 << bits) - 1`, and every reading looks
//! pinned at maximum: wrong by a factor of sixteen, and entirely plausible.
//! Work in 16 bits.

#![no_main]
#![no_std]

use core::fmt::Write;

use libtock::adc::Adc;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x800}

/// GP26 on the kit.
const X: u32 = 0;
/// GP27 on the kit.
const Y: u32 = 1;

/// Samples averaged at startup to find the rest position.
const CALIBRATION_SAMPLES: u32 = 8;

/// How far from centre counts as a deliberate push, as a fraction of the whole
/// 16-bit range. Generous, because a stick at rest wanders by a few hundred.
const DEADZONE: i32 = 6000;

fn main() {
    let mut console = Console::writer();

    let channels = match Adc::count() {
        Ok(count) => count,
        Err(_) => {
            let _ = writeln!(console, "joystick: no ADC driver on this kernel");
            return;
        }
    };

    if channels < 2 {
        let _ = writeln!(
            console,
            "joystick: the board exposes {channels} ADC channel(s), and this needs 2"
        );
        return;
    }

    let bits = Adc::get_resolution_bits().unwrap_or(0);
    let reference = Adc::get_reference_voltage_mv().unwrap_or(0);
    let _ = writeln!(
        console,
        "joystick: {channels} channels, {bits} bits, {reference} mV reference"
    );

    let (centre_x, centre_y) = match (calibrate(X), calibrate(Y)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let _ = writeln!(console, "joystick: could not read both channels");
            return;
        }
    };
    let _ = writeln!(
        console,
        "joystick: resting at ch{X}={centre_x} ch{Y}={centre_y}; move the stick"
    );

    // The travel, accumulated as the stick is moved. Starts at the rest
    // position and widens, so it reports what has actually been reached rather
    // than what the hardware is supposed to manage.
    let (mut min_x, mut max_x) = (centre_x, centre_x);
    let (mut min_y, mut max_y) = (centre_y, centre_y);

    loop {
        let (x, y) = match (sample(X), sample(Y)) {
            (Some(x), Some(y)) => (x, y),
            _ => {
                let _ = writeln!(console, "joystick: a read failed, stopping");
                return;
            }
        };

        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);

        let dx = x as i32 - centre_x as i32;
        let dy = y as i32 - centre_y as i32;

        let _ = writeln!(
            console,
            "ch{X} {x:5} ({dx:+6})  ch{Y} {y:5} ({dy:+6})  {:<11} travel ch{X} {min_x}..{max_x} ch{Y} {min_y}..{max_y}",
            describe(dx, dy)
        );

        if Alarm::sleep_for(Milliseconds(200)).is_err() {
            let _ = writeln!(console, "joystick: no alarm driver");
            return;
        }
    }
}

/// One sample, or `None` if the channel could not be read.
fn sample(channel: u32) -> Option<u16> {
    Adc::read_single_sample_sync(channel).ok()
}

/// The rest position of `channel`, averaged so a noisy sample does not become
/// the centre for the whole run.
fn calibrate(channel: u32) -> Option<u16> {
    let mut total: u32 = 0;
    for _ in 0..CALIBRATION_SAMPLES {
        total += sample(channel)? as u32;
    }
    Some((total / CALIBRATION_SAMPLES) as u16)
}

/// Names the push in terms of the channels, not of north and south: which axis
/// is horizontal, and which way round it is, depends on how the stick is
/// mounted, and this example is not in a position to know.
fn describe(dx: i32, dy: i32) -> &'static str {
    let x = if dx > DEADZONE {
        1
    } else if dx < -DEADZONE {
        -1
    } else {
        0
    };
    let y = if dy > DEADZONE {
        1
    } else if dy < -DEADZONE {
        -1
    } else {
        0
    };

    match (x, y) {
        (0, 0) => "centred",
        (1, 0) => "ch0 high",
        (-1, 0) => "ch0 low",
        (0, 1) => "ch1 high",
        (0, -1) => "ch1 low",
        (1, 1) => "both high",
        (-1, -1) => "both low",
        (1, -1) => "ch0 high/ch1 low",
        _ => "ch0 low/ch1 high",
    }
}
