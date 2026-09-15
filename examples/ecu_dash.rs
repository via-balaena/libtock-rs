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

/// The VERTICAL stick axis: straight up accelerates, released coasts, pulled
/// back regenerates. Inferred rather than measured -- the throttle used to
/// peak at full right on ADC0, which is what a horizontal axis does, so
/// vertical is the other one. The range report below confirms or refutes it
/// on the first push.
const PEDAL_CH: u32 = 1;
/// The other axis. Read, never acted on, reported -- so a wrong guess above
/// shows up as a channel that does not move.
const OTHER_CH: u32 = 0;
const BRAKE_BTN: u32 = 0;

// THE PLAUSIBILITY BAND BELONGS TO THE SENSOR, NOT TO THE THROTTLE. A hall
// throttle sits inside 0.8-4.2 V of a 5 V rail, so 0 V is a broken wire and
// 5 V is a short, and neither is a pedal position. A bare potentiometer --
// which is what a joystick axis is -- reaches BOTH rails as part of its
// normal travel, measured 416..65520, so the vehicle's band reads full
// deflection as a short. That is exactly how it presented: the throttle
// opened near the 45 degree diagonal and dropped out past it, where the axis
// crossed 64,000.
//
// These are bench values. Be exact about what that means: `raw` is a u16 and
// BAND_HIGH is u16::MAX, so the upper branch can never be taken -- the
// ceiling is OFF, not widened, and the vehicle failure it exists to catch (an
// open circuit reading full scale) is precisely the one it cannot see. Say
// "off" rather than "wide" so restoring it does not read as a tuning nicety.
// Caught by the libtock-rs session, 2026-09-15.
// TWO RESISTORS FIX IT PROPERLY: 2k from 3V3 to the stick module's VCC and 2k
// from its GND pin to ground compresses BOTH axes into roughly 14%..86% of
// the range, which puts the rails back out of reach and makes 4_000 / 62_000
// the right band. Plus 100k from each wiper to ground, because an OPEN wiper
// floats to somewhere near mid-scale and reads as a valid half throttle,
// which is the failure that matters.
const BAND_LOW: u16 = 100;
const BAND_HIGH: u16 = 65_535;

/// Centre is sampled at boot, which doubles as the zero-at-boot interlock:
/// the stick has to be released for the ECU to come up. A sample outside this
/// window is a stuck stick or a broken sensor, and the throttle stays shut
/// for the life of the process.
const CENTRE_MIN: u16 = 25_000;
const CENTRE_MAX: u16 = 40_000;
/// Either side of centre, where the stick is treated as released. Wider than
/// the 208 counts of jitter measured at rest, so a resting hand does not
/// creep the throttle open.
const DEADBAND: u16 = 2_500;
/// Top of useful travel. Measured 65,520 at the rail; back off so the last
/// stretch of throw is full throttle rather than a fault.
const PEDAL_FULL: u16 = 62_000;
/// Bottom of useful travel, for the regen half.
const PEDAL_MIN: u16 = 2_000;

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
    // ONE CALL, NO RETRY. The panel answers BUSY to any frame call until its
    // init sequence finishes -- 1,239 ms after boot, measured on the kit's
    // ST7796 -- and nothing in the syscall API tells an app that:
    // `Screen::exists()` and `get_resolution()` are answered by the capsule
    // without touching the driver, so they succeed immediately. This app used
    // to carry a retry loop, and picking its budget was the hard part: one
    // that gave up at 1,017 ms still failed.
    //
    // The capsule queues the command now and serves it from
    // `ScreenClient::screen_is_ready` instead, so the wait happens inside
    // `yield_wait` and the app does not need to know the panel exists.
    let hz = Alarm::get_frequency().map(|f| f.0).unwrap_or(1);
    let t0 = Alarm::get_ticks().unwrap_or(0);
    let frame = Screen::set_write_frame(0, 0, w, h);
    let mut buf = [0u8; 2];
    let filled = if frame.is_ok() {
        Screen::fill(&mut buf, BG)
    } else {
        frame
    };
    let t1 = Alarm::get_ticks().unwrap_or(0);
    let _ = writeln!(
        console,
        "dash: clear {w}x{h} frame={frame:?} fill={filled:?} in {} ms",
        t1.wrapping_sub(t0) as u64 * 1000 / hz.max(1) as u64,
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

    // Sample centre with the stick released. This IS the zero-at-boot
    // interlock: a reading outside the window is a stuck stick or a broken
    // sensor, and the throttle then stays shut for the life of the process.
    let mut sum: u32 = 0;
    for _ in 0..16 {
        sum += Adc::read_single_sample_sync(PEDAL_CH).unwrap_or(0) as u32;
        let _ = Alarm::sleep_for(Milliseconds(10));
    }
    let centre = (sum / 16) as u16;
    let calibrated = (CENTRE_MIN..=CENTRE_MAX).contains(&centre);
    let _ = writeln!(
        console,
        "dash: centre {centre} on ch{PEDAL_CH} -- {}",
        if calibrated {
            "armed"
        } else {
            "OUT OF RANGE, throttle stays shut"
        }
    );

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
        let raw = Adc::read_single_sample_sync(PEDAL_CH).unwrap_or(centre);
        let other = Adc::read_single_sample_sync(OTHER_CH).unwrap_or(0);
        lo0 = lo0.min(raw);
        hi0 = hi0.max(raw);
        lo1 = lo1.min(other);
        hi1 = hi1.max(other);
        let faulted = !(BAND_LOW..=BAND_HIGH).contains(&raw);

        // Released means back inside the deadband, not "below centre" -- the
        // lower half of a centring stick is regen, not extra idle. Coming
        // back to centre is what clears a brake lockout.
        let at_rest = raw.abs_diff(centre) <= DEADBAND;
        if at_rest {
            ever_at_rest = true;
            locked_out = false;
        }
        let braking = Buttons::is_pressed(BRAKE_BTN);
        if braking {
            locked_out = true;
        }

        let shut = faulted || braking || locked_out || !ever_at_rest || !calibrated;
        let up = centre.saturating_add(DEADBAND);
        let target = if shut || raw <= up {
            0
        } else {
            let span = (PEDAL_FULL.saturating_sub(up)).max(1) as u32;
            (raw.min(PEDAL_FULL) - up) as u32 * SCALE / span
        };

        // The other half of one-pedal driving. Reported only: braking is a
        // controller command and there is no controller on this bench, so
        // nothing here may quietly become an output.
        let down = centre.saturating_sub(DEADBAND);
        let regen = if shut || raw >= down {
            0
        } else {
            let span = (down.saturating_sub(PEDAL_MIN)).max(1) as u32;
            (down - raw.max(PEDAL_MIN)) as u32 * SCALE / span
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
                 regen={regen} centre={centre} \
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
