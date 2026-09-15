//! The trike ECU with a dash, closing the loop on the bench.
//!
//! Everything the ECU does, in one process, with the parts the breadboard kit
//! already has:
//!
//! ```text
//!   joystick (ADC0)  -->  throttle capsule  -->  GP19
//!        button GP14  -->       |
//!                              (this app also drives GP20 at a frequency
//!                               proportional to the throttle, standing in
//!                               for a wheel that speeds up when you open it)
//!                                     GP20 --jumper--> GP21
//!                                                        |
//!                                              pulse counter --> the dash
//! ```
//!
//! The simulated wheel is the point of the loop. A dash fed from a number the
//! same app just computed shows only that the app can print. A dash fed from a
//! pulse train that left the chip, crossed a wire and was counted by a
//! different capsule shows that the whole path works, and it is the same path
//! a hall sensor will use.
//!
//! # Drawing without a font
//!
//! Every element is a filled rectangle: `set_write_frame` then `fill`, which
//! needs a two byte buffer because the kernel repeats the colour across the
//! frame. Seven segments make a digit. That keeps the app's RAM in the
//! hundreds of bytes rather than the 300 KB a framebuffer for this panel would
//! take, and it means the dash costs about twenty small writes per update
//! rather than a full-screen blit.
//!
//! Build the board with `kit_display`, `kit_input` and `wheel_source`.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::adc::Adc;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buttons::Buttons;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock::display::Screen;
use libtock_platform::{ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x800}

const THROTTLE: u32 = 0x00012;
const T_ARM: u32 = 1;
const T_SET: u32 = 2;
const T_ACTUAL: u32 = 4;

const COUNTER: u32 = 0x00013;
const C_START: u32 = 1;
const C_RATE: u32 = 3;

const PWM: u32 = 0x00010;
const PWM_START: u32 = 1;
const PWM_STOP: u32 = 2;
/// PWM index 1 is GP20, the jumper's far end.
const WHEEL_PIN: u32 = 1;

const SCALE: u32 = 10_000;

// RGB565.
const BG: u16 = 0x0000;
const DIM: u16 = 0x2104;
const GREEN: u16 = 0x07E0;
const AMBER: u16 = 0xFD20;
const RED: u16 = 0xF800;
const WHITE: u16 = 0xFFFF;

const PEDAL_CH: u32 = 0;
const BRAKE_BTN: u32 = 0;
const BAND_LOW: u16 = 1_500;
const BAND_HIGH: u16 = 64_000;
const PEDAL_REST: u16 = 33_000;
const PEDAL_FULL: u16 = 60_000;

fn rect(x: u32, y: u32, w: u32, h: u32, colour: u16) {
    if w == 0 || h == 0 {
        return;
    }
    let mut buf = [0u8; 2];
    if Screen::set_write_frame(x, y, w, h).is_ok() {
        let _ = Screen::fill(&mut buf, colour);
    }
}

/// Which of the seven segments each digit lights, in the order
/// a, b, c, d, e, f, g.
const SEGMENTS: [[bool; 7]; 10] = [
    [true, true, true, true, true, true, false],      // 0
    [false, true, true, false, false, false, false],  // 1
    [true, true, false, true, true, false, true],     // 2
    [true, true, true, true, false, false, true],     // 3
    [false, true, true, false, false, true, true],    // 4
    [true, false, true, true, false, true, true],     // 5
    [true, false, true, true, true, true, true],      // 6
    [true, true, true, false, false, false, false],   // 7
    [true, true, true, true, true, true, true],       // 8
    [true, true, true, true, false, true, true],      // 9
];

/// Draw one digit, lighting segments in `on` and painting the rest in `off`
/// so the previous value is erased without clearing the whole area first.
fn digit(x: u32, y: u32, w: u32, h: u32, t: u32, value: Option<u8>, on: u16, off: u16) {
    let lit = match value {
        Some(v) if (v as usize) < 10 => SEGMENTS[v as usize],
        _ => [false; 7],
    };
    let half = h / 2;
    let boxes = [
        (x, y, w, t),                    // a  top
        (x + w - t, y, t, half),         // b  upper right
        (x + w - t, y + half, t, half),  // c  lower right
        (x, y + h - t, w, t),            // d  bottom
        (x, y + half, t, half),          // e  lower left
        (x, y, t, half),                 // f  upper left
        (x, y + half - t / 2, w, t),     // g  middle
    ];
    for (i, (bx, by, bw, bh)) in boxes.iter().enumerate() {
        rect(*bx, *by, *bw, *bh, if lit[i] { on } else { off });
    }
}

fn main() {
    let mut console = Console::writer();

    if Screen::exists().is_err() {
        let _ = writeln!(console, "dash: no screen -- build with kit_display");
        return;
    }
    let (w, h) = Screen::get_resolution().unwrap_or((480, 320));
    let _ = writeln!(console, "dash: {w}x{h}");

    // Paint the whole panel once. Everything after this is small updates.
    rect(0, 0, w, h, BG);

    let _ = TockSyscalls::command(COUNTER, C_START, 0, 0).to_result::<(), ErrorCode>();
    let _ = TockSyscalls::command(THROTTLE, T_ARM, 0, 0).to_result::<(), ErrorCode>();

    let mut ever_at_rest = false;
    let mut locked_out = false;

    loop {
        let raw = Adc::read_single_sample_sync(PEDAL_CH).unwrap_or(PEDAL_REST);
        let faulted = raw < BAND_LOW || raw > BAND_HIGH;

        let at_rest = raw <= PEDAL_REST;
        if at_rest {
            ever_at_rest = true;
            locked_out = false;
        }
        let braking = Buttons::is_pressed(BRAKE_BTN);
        if braking {
            locked_out = true;
        }

        let target = if faulted || braking || locked_out || !ever_at_rest || at_rest {
            0
        } else {
            let span = (PEDAL_FULL - PEDAL_REST) as u32;
            (raw.min(PEDAL_FULL) - PEDAL_REST) as u32 * SCALE / span
        };
        let _ = TockSyscalls::command(THROTTLE, T_SET, target, 0).to_result::<(), ErrorCode>();

        let actual = TockSyscalls::command(THROTTLE, T_ACTUAL, 0, 0)
            .to_result::<u32, ErrorCode>()
            .unwrap_or(0);

        // The wheel. Frequency rises with the throttle, so the number on the
        // dash comes back through a wire rather than from this loop.
        if actual == 0 {
            let _ = TockSyscalls::command(PWM, PWM_STOP, WHEEL_PIN, 0).to_result::<(), ErrorCode>();
        } else {
            let hz = 40 + actual * 960 / SCALE;
            let packed = WHEEL_PIN | (5000 << 16);
            let _ =
                TockSyscalls::command(PWM, PWM_START, packed, hz).to_result::<(), ErrorCode>();
        }

        let pps = TockSyscalls::command(COUNTER, C_RATE, 0, 0)
            .to_result::<u32, ErrorCode>()
            .unwrap_or(0);

        // Speed, as three segment digits. Leading zeros are blanked, which is
        // what makes it read as a number rather than an odometer.
        let shown = pps.min(999);
        let (d0, d1, d2) = (shown / 100, (shown / 10) % 10, shown % 10);
        let colour = if faulted {
            RED
        } else if shown > 700 {
            AMBER
        } else {
            GREEN
        };
        digit(30, 40, 100, 160, 18, if d0 > 0 { Some(d0 as u8) } else { None }, colour, BG);
        digit(
            150, 40, 100, 160, 18,
            if shown >= 10 { Some(d1 as u8) } else { None },
            colour, BG,
        );
        digit(270, 40, 100, 160, 18, Some(d2 as u8), colour, BG);

        // Throttle, as a bar across the bottom.
        let bar_w = w - 60;
        let filled = bar_w * actual / SCALE;
        rect(30, 240, filled, 40, if braking { RED } else { GREEN });
        rect(30 + filled, 240, bar_w - filled, 40, DIM);

        // State, top right: red while the throttle is locked out or faulted,
        // white while it is merely closed, green when it is free to open.
        let state = if faulted || braking || locked_out || !ever_at_rest {
            RED
        } else if actual == 0 {
            WHITE
        } else {
            GREEN
        };
        rect(w - 60, 30, 40, 40, state);

        let _ = Alarm::sleep_for(Milliseconds(100));
    }
}
