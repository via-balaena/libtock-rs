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
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};
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

/// Frequency the preflight drives the wheel source at. Far enough above the
/// counter's 4 pps resolution that "heard nothing" and "heard it" cannot be
/// confused, and inside the range a real wheel will reach.
const PREFLIGHT_HZ: u32 = 400;

const PEDAL_CH: u32 = 0;
/// The other stick axis. Read but never acted on -- it is here so the console
/// can say what each channel actually does, which the Doom port's note
/// ("ch1 vertical, ch0 horizontal") turned out not to settle: the throttle
/// peaks at the 45 degree diagonal, and a plain horizontal axis would peak at
/// full right.
const OTHER_CH: u32 = 1;
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
    [true, true, true, true, true, true, false],     // 0
    [false, true, true, false, false, false, false], // 1
    [true, true, false, true, true, false, true],    // 2
    [true, true, true, true, false, false, true],    // 3
    [false, true, true, false, false, true, true],   // 4
    [true, false, true, true, false, true, true],    // 5
    [true, false, true, true, true, true, true],     // 6
    [true, true, true, false, false, false, false],  // 7
    [true, true, true, true, true, true, true],      // 8
    [true, true, true, true, false, true, true],     // 9
];

/// All three speed digits share one geometry, so it lives here rather than in
/// the signature.
const DIGIT_Y: u32 = 40;
const DIGIT_W: u32 = 100;
const DIGIT_H: u32 = 160;
const DIGIT_T: u32 = 18;

/// Draw one digit, lighting segments in `on` and painting the rest in `off`
/// so the previous value is erased without clearing the whole area first.
fn digit(x: u32, value: Option<u8>, on: u16, off: u16) {
    let (y, w, h, t) = (DIGIT_Y, DIGIT_W, DIGIT_H, DIGIT_T);
    let lit = match value {
        Some(v) if (v as usize) < 10 => SEGMENTS[v as usize],
        _ => [false; 7],
    };
    let half = h / 2;
    let boxes = [
        (x, y, w, t),                   // a  top
        (x + w - t, y, t, half),        // b  upper right
        (x + w - t, y + half, t, half), // c  lower right
        (x, y + h - t, w, t),           // d  bottom
        (x, y + half, t, half),         // e  lower left
        (x, y, t, half),                // f  upper left
        (x, y + half - t / 2, w, t),    // g  middle
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
    //
    // THE FIRST DRAW HAS TO WAIT, AND NOTHING TELLS YOU SO. Measured on a
    // Pico 2 W with the kit's ST7796: every `set_write_frame` answers BUSY
    // for the first **1,246 ms** after boot, then works, and the full-screen
    // fill that follows takes 81 ms. Both BUSY sources in
    // `capsules/extra/src/screen/screen.rs:149` are the app's own
    // `pending_command`, not the panel -- and `Screen::exists()` and
    // `get_resolution()` are answered by the capsule without touching the
    // driver, so they succeed immediately and tell an app nothing. There is
    // no "screen ready" signal in the syscall API.
    //
    // An app that clears once and drops the error -- which is what a helper
    // like `rect` below quietly does -- leaves the PREVIOUS app's frozen
    // image showing through everywhere it does not draw. That is exactly how
    // this dash first came up: a working dash rendered over a still frame of
    // Doom. Retry until it takes.
    let hz = Alarm::get_frequency().map(|f| f.0).unwrap_or(1);
    let t0 = Alarm::get_ticks().unwrap_or(0);
    let mut attempts = 0u32;
    let mut frame: Result<(), ErrorCode> = Err(ErrorCode::Busy);
    while attempts < 200 {
        attempts += 1;
        frame = Screen::set_write_frame(0, 0, w, h);
        if frame.is_ok() {
            break;
        }
        let _ = Alarm::sleep_for(Milliseconds(25));
    }
    let t1 = Alarm::get_ticks().unwrap_or(0);
    let mut buf = [0u8; 2];
    let fill = if frame.is_ok() {
        Screen::fill(&mut buf, BG)
    } else {
        Err(ErrorCode::Busy)
    };
    let t2 = Alarm::get_ticks().unwrap_or(0);
    let ms = |a: u32, b: u32| b.wrapping_sub(a) as u64 * 1000 / hz.max(1) as u64;
    let _ = writeln!(
        console,
        "dash: clear frame={frame:?} after {attempts} tries / {} ms, \
         fill={fill:?} in {} ms",
        ms(t0, t1),
        ms(t1, t2),
    );

    let _ = TockSyscalls::command(COUNTER, C_START, 0, 0).to_result::<(), ErrorCode>();

    // Preflight. Drive the wheel source at a frequency this app chose and ask
    // the counter what it heard. That covers the jumper, the pad mux and the
    // counter in one step, BEFORE the dash starts showing a speed that depends
    // on all three -- a dash reading zero is otherwise the same picture whether
    // the wheel is stopped or the wire is off.
    let started = TockSyscalls::command(PWM, PWM_START, WHEEL_PIN | (5000 << 16), PREFLIGHT_HZ)
        .to_result::<(), ErrorCode>();
    // Two counter windows, so the reading is a full window rather than a
    // partial one that would read low and look like a fault.
    let _ = Alarm::sleep_for(Milliseconds(600));
    let heard = TockSyscalls::command(COUNTER, C_RATE, 0, 0)
        .to_result::<u32, ErrorCode>()
        .unwrap_or(0);
    let _ = TockSyscalls::command(PWM, PWM_STOP, WHEEL_PIN, 0).to_result::<(), ErrorCode>();
    let wheel_ok = started.is_ok() && heard.abs_diff(PREFLIGHT_HZ) <= PREFLIGHT_HZ / 10;
    let _ = writeln!(
        console,
        "dash: preflight {PREFLIGHT_HZ} Hz -> {heard} pps -- {}",
        if wheel_ok {
            "wheel path ok"
        } else {
            "NO WHEEL SIGNAL, check the GP20-GP21 jumper"
        }
    );
    // Top left, painted once and never redrawn: green if the speed on this
    // dash came through a wire, amber if the number is meaningless.
    rect(20, 8, 24, 20, if wheel_ok { GREEN } else { AMBER });

    let _ = TockSyscalls::command(THROTTLE, T_ARM, 0, 0).to_result::<(), ErrorCode>();

    let mut ever_at_rest = false;
    let mut locked_out = false;
    let mut tick: u32 = 0;
    let mut wheel_on = false;
    // Highest value each has ever reached. The joystick is only moved when
    // somebody is at the bench, and the console is only read when somebody is
    // not -- a once-a-second sample of a live value never catches the two
    // together.
    let (mut pk_pedal, mut pk_target, mut pk_actual, mut pk_pps) = (0u16, 0u32, 0u32, 0u32);
    // Highest wheel frequency ever commanded, and the highest pps seen in
    // each quarter of the throttle range -- one stick sweep then leaves the
    // whole throttle-to-wheel transfer behind for whoever reads the console
    // next. The counter's 250 ms window smears this while the throttle is
    // moving, in both directions; a band is only trustworthy for a position
    // that was HELD.
    let mut pk_hz = 0u32;
    // Range of both stick axes, so the console can report what the hardware
    // does rather than what a previous note said it did.
    let (mut lo0, mut hi0, mut lo1, mut hi1) = (u16::MAX, 0u16, u16::MAX, 0u16);
    let mut band_pps = [0u32; 4];
    let mut set_errs: u32 = 0;
    let mut wheel_errs: u32 = 0;

    loop {
        let raw = Adc::read_single_sample_sync(PEDAL_CH).unwrap_or(PEDAL_REST);
        let other = Adc::read_single_sample_sync(OTHER_CH).unwrap_or(0);
        lo0 = lo0.min(raw);
        hi0 = hi0.max(raw);
        lo1 = lo1.min(other);
        hi1 = hi1.max(other);
        let faulted = !(BAND_LOW..=BAND_HIGH).contains(&raw);

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
        if TockSyscalls::command(THROTTLE, T_SET, target, 0)
            .to_result::<(), ErrorCode>()
            .is_err()
        {
            set_errs = set_errs.wrapping_add(1);
        }

        let actual = TockSyscalls::command(THROTTLE, T_ACTUAL, 0, 0)
            .to_result::<u32, ErrorCode>()
            .unwrap_or(0);

        // The wheel. Frequency rises with the throttle, so the number on the
        // dash comes back through a wire rather than from this loop.
        // Stopping a pin that is not running is ErrorCode::OFF (pwm.rs:175), so
        // the stop goes on the transition only. Without that the closed
        // throttle produced ten failed syscalls a second, which buried a real
        // failure of this call in noise.
        let want_wheel = actual > 0;
        let wheel = if want_wheel {
            let hz = 40 + actual * 960 / SCALE;
            pk_hz = pk_hz.max(hz);
            let packed = WHEEL_PIN | (5000 << 16);
            TockSyscalls::command(PWM, PWM_START, packed, hz).to_result::<(), ErrorCode>()
        } else if wheel_on {
            TockSyscalls::command(PWM, PWM_STOP, WHEEL_PIN, 0).to_result::<(), ErrorCode>()
        } else {
            Ok(())
        };
        match wheel {
            Ok(()) => wheel_on = want_wheel,
            Err(_) => wheel_errs = wheel_errs.wrapping_add(1),
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
        digit(30, if d0 > 0 { Some(d0 as u8) } else { None }, colour, BG);
        digit(
            150,
            if shown >= 10 { Some(d1 as u8) } else { None },
            colour,
            BG,
        );
        digit(270, Some(d2 as u8), colour, BG);

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

        // One line a second. The glass carries the same numbers, but a bench
        // session driven over SSH cannot see the glass, and a dash that only
        // draws is indistinguishable from a dash that has stopped drawing.
        pk_pedal = pk_pedal.max(raw);
        let band = ((actual * 4 / (SCALE + 1)) as usize).min(3);
        if actual > 0 {
            band_pps[band] = band_pps[band].max(pps);
        }
        pk_target = pk_target.max(target);
        pk_actual = pk_actual.max(actual);
        pk_pps = pk_pps.max(pps);

        tick = tick.wrapping_add(1);
        if tick % 10 == 0 {
            let _ = writeln!(
                console,
                "dash: pedal={raw} target={target} actual={actual} pps={pps} \
                 peak={pk_pedal}/{pk_target}/{pk_actual}/{pk_pps} hz={pk_hz} \
                 curve={}/{}/{}/{} \
                 ch0={lo0}..{hi0} ch1={lo1}..{hi1} \
                 set_err={set_errs} wheel_err={wheel_errs} {}{}{}{}",
                band_pps[0],
                band_pps[1],
                band_pps[2],
                band_pps[3],
                if faulted { "FAULT " } else { "" },
                if braking { "BRAKE " } else { "" },
                if locked_out { "LOCKOUT " } else { "" },
                if ever_at_rest { "" } else { "NEEDS-REST" },
            );
        }

        let _ = Alarm::sleep_for(Milliseconds(100));
    }
}
