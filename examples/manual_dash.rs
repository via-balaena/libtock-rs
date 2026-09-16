//! A manual gearbox you drive with the joystick, where **lifting off is the
//! clutch**.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=manual_dash
//! ```
//!
//! Needs a kernel with `kit_display` and `kit_input`.
//!
//! # The one rule that makes it a game
//!
//! **A shift only takes if the throttle is below [`SHIFT_LIFT`].** Ask for a
//! gear with the power on and the box does not take it — it drops to neutral,
//! the revs flare, and you have to lift, select and get back on it. That is the
//! whole feel: lift, shift, power, and a missed shift costs you the corner.
//!
//! Neutral was chosen over an over-rev for the penalty because it is legible
//! without animation — the gear character changes, drive disappears, and the
//! cause is obvious from one glance at the glass. An over-rev needs a limiter
//! flash to read as anything at all.
//!
//! # Controls
//!
//!| input | what |
//! |---|---|
//! | joystick Y, ADC ch 1 | throttle, analogue, centred at startup |
//! | button 0 = GP14, the RIGHT one | shift down |
//! | button 1 = GP15, the LEFT one | shift up |
//!
//! The button array is `[GPIO14, GPIO15]` in the board's own order, so the
//! indices are not a guess — but the telemetry line prints both states anyway,
//! so a swapped pair is visible in the transcript rather than needing someone
//! to be watching at the right moment.
//!
//! # Why the display is segments and not a needle
//!
//! Measured wire time on this panel: a swept needle redrawing a 200x200 box is
//! about 10.2 ms of SPI *before* any syscall overhead, a third of a 30 fps
//! frame for one gauge. A segment that changed is 30x30 — about 0.23 ms, some
//! 40x cheaper for the same information, and it degrades into staleness rather
//! than into a needle pointing somewhere wrong. So: draw everything once, then
//! touch only what changed. `screen_bench` is the instrument those came from.
//!
//! Every digit is seven rectangles and only the segments that differ between
//! the old value and the new one are redrawn, so a speed ticking 88 -> 89
//! costs two rectangles, not a glyph.
//!
//! # What is deliberately absent
//!
//! **No engine sound.** The board has a PWM buzzer and a rising note would make
//! this land much harder, but it is audible, there is an unexplained stuck-tone
//! on record, and nothing audible goes on this bench without Jon saying so.
//!
//! This is a driving-feel simulator and is deliberately **not** an evolution of
//! `ecu_dash`, which is real ECU work with a live throttle path. Nothing here
//! commands a motor.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::adc::Adc;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buttons::Buttons;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x1000}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Joystick Y. Same channel `ecu_dash` centres on.
const THROTTLE_CH: u32 = 1;
/// Board button array order is `[GPIO14, GPIO15]`.
const SHIFT_DOWN_BTN: u32 = 0;
const SHIFT_UP_BTN: u32 = 1;

/// Throttle is 0..=`THROTTLE_FULL` after centring.
const THROTTLE_FULL: u32 = 1_000;
/// Above this, the box refuses a shift. About a quarter throttle.
const SHIFT_LIFT: u32 = 250;
/// Joystick travel above centre that counts as full throttle. Measured on this
/// kit: the stick reaches both rails, so this is a fraction of the span rather
/// than the rail itself.
const STICK_SPAN: u32 = 28_000;

// ---------------------------------------------------------------------------
// Drivetrain. All tunable, none of it sacred.
// ---------------------------------------------------------------------------

/// Gear ratios x100, first to sixth. Index 0 is neutral and unused here.
const RATIOS: [u32; 7] = [0, 360, 210, 140, 100, 80, 65];
const FINAL_DRIVE: u32 = 340;
const TOP_GEAR: usize = 6;

/// Chosen so first gear redlines around 40 km/h and sixth around 220.
const RPM_DIVISOR: u64 = 194_000;

const IDLE_RPM: u32 = 800;
const REDLINE_RPM: u32 = 7_000;
/// Below this in gear, it stalls.
const STALL_RPM: u32 = 500;
/// Where the shift light comes on.
const SHIFT_LIGHT_RPM: u32 = 6_200;

/// Physics tick. 100 Hz — comfortable, since an app's floor is 153 us and the
/// 10 ms excursions only appear behind a CPU-bound sibling, which this has not.
const TICK_MS: u32 = 10;

/// Drag. Linear term is rolling resistance, square term is aero. Both are
/// FORCES, in the same units as drive -- see `apply_drag`.
const DRAG_LINEAR: u64 = 90;
const DRAG_SQUARE: u64 = 440;
/// Bigger is heavier: divides NET force before it becomes acceleration.
/// Tuned against a host simulation of this exact integer arithmetic: first
/// gear 0-40 km/h in 1.9 s, 0-100 in 8.3 s, 202 km/h terminal in sixth.
const INERTIA: u64 = 165_000;

/// How fast free revs chase the throttle in neutral, and decay without it.
const NEUTRAL_RISE: u32 = 420;
const NEUTRAL_FALL: u32 = 260;

// ---------------------------------------------------------------------------
// Palette and layout
// ---------------------------------------------------------------------------

const BG: u16 = 0x0000;
const DIM: u16 = 0x2104;
const GREEN: u16 = 0x07E0;
const AMBER: u16 = 0xFD20;
const RED: u16 = 0xF800;
const WHITE: u16 = 0xFFFF;
const BLUE: u16 = 0x041F;

/// Tach bar: segments across the top.
const TACH_SEGS: u32 = 16;
const TACH_X: u32 = 8;
const TACH_Y: u32 = 12;
const TACH_W: u32 = 28;
const TACH_H: u32 = 44;
const TACH_GAP: u32 = 2;

const LIGHT_Y: u32 = 66;
const LIGHT_H: u32 = 14;

/// Big gear character.
const GEAR_X: u32 = 34;
const GEAR_Y: u32 = 110;
const GEAR_W: u32 = 110;
const GEAR_H: u32 = 170;
const GEAR_T: u32 = 22;

/// Three speed digits.
const SPD_X: u32 = 250;
const SPD_Y: u32 = 150;
const SPD_W: u32 = 58;
const SPD_H: u32 = 100;
const SPD_T: u32 = 13;
const SPD_GAP: u32 = 14;

/// Seven-segment masks, bit 0 = A (top) round to bit 6 = G (middle).
const DIGITS: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];
/// A blocky `N`: both uprights plus the top bar.
const GLYPH_N: u8 = 0x37;
/// Nothing lit — used for a blanked leading digit.
const GLYPH_BLANK: u8 = 0x00;

fn main() {
    let mut console = Console::writer();

    if Screen::exists().is_err() {
        let _ = writeln!(
            console,
            "manual_dash: no screen driver at 0x90001; needs the kit_display \
             board feature.\r"
        );
        return;
    }

    // Centre the stick before anything depends on it. Same approach as
    // ecu_dash: the joystick rests wherever it rests.
    let mut sum: u32 = 0;
    for _ in 0..32 {
        sum += Adc::read_single_sample_sync(THROTTLE_CH).unwrap_or(0) as u32;
        let _ = Alarm::sleep_for(Milliseconds(2));
    }
    let centre = sum / 32;

    // This first draw carries the panel's readiness gap -- roughly 1.24 s --
    // because the capsule queues a command it is not ready for and serves it
    // when it is. Everything after it is cheap.
    let (w, h) = Screen::get_resolution().unwrap_or((480, 320));
    rect(0, 0, w, h, BG);

    let _ = writeln!(
        console,
        "manual_dash: {w}x{h}, stick centred at {centre} on ch{THROTTLE_CH}\r\n\
         \x20 button {SHIFT_UP_BTN} (GP15, left) = UP, button {SHIFT_DOWN_BTN} \
         (GP14, right) = DOWN\r\n\
         \x20 a shift only takes below {SHIFT_LIFT}/{THROTTLE_FULL} throttle -- \
         ask with the power on\r\n\
         \x20 and it drops to neutral. Lift, shift, power.\r"
    );

    // Static furniture, drawn once.
    for i in 0..TACH_SEGS {
        rect(seg_x(i), TACH_Y, TACH_W, TACH_H, DIM);
    }

    let mut st = State {
        gear: 0,
        rpm: IDLE_RPM,
        speed: 0,
        stalled: false,
    };

    // Everything the display is currently showing, so only differences are
    // drawn. Deliberately impossible starting values so the first frame paints.
    let mut shown_segs: u32 = u32::MAX;
    let mut shown_gear_mask: u8 = 0xFF;
    let mut shown_spd: [u8; 3] = [0xFF; 3];
    let mut shown_light: u16 = 1;

    let mut up_was = false;
    let mut down_was = false;
    let mut missed_for = 0u32;
    let mut tick: u32 = 0;

    loop {
        let raw = Adc::read_single_sample_sync(THROTTLE_CH).unwrap_or(centre as u16) as u32;
        let throttle = throttle_from(raw, centre);

        let up_now = Buttons::is_pressed(SHIFT_UP_BTN);
        let down_now = Buttons::is_pressed(SHIFT_DOWN_BTN);
        let want_up = up_now && !up_was;
        let want_down = down_now && !down_was;
        up_was = up_now;
        down_was = down_now;

        if want_up || want_down {
            if throttle >= SHIFT_LIFT {
                // Refused. The box does not take it and you lose drive.
                st.gear = 0;
                st.stalled = false;
                missed_for = 60;
            } else if want_up {
                if st.gear < TOP_GEAR {
                    st.gear += 1;
                }
                st.stalled = false;
            } else if st.gear > 0 {
                st.gear -= 1;
                st.stalled = false;
            }
        }

        st.step(throttle);

        // ---- display, differences only --------------------------------------

        let lit = (st.rpm.min(REDLINE_RPM) * TACH_SEGS) / REDLINE_RPM;
        if lit != shown_segs {
            let (from, to) = if lit > shown_segs.min(TACH_SEGS) {
                (shown_segs.min(TACH_SEGS), lit)
            } else {
                (lit, shown_segs.min(TACH_SEGS))
            };
            for i in from..to.min(TACH_SEGS) {
                rect(
                    seg_x(i),
                    TACH_Y,
                    TACH_W,
                    TACH_H,
                    if i < lit { tach_colour(i) } else { DIM },
                );
            }
            shown_segs = lit;
        }

        let light = if st.stalled {
            BLUE
        } else if st.rpm >= REDLINE_RPM {
            RED
        } else if st.rpm >= SHIFT_LIGHT_RPM {
            AMBER
        } else if missed_for > 0 {
            RED
        } else {
            BG
        };
        if light != shown_light {
            rect(0, LIGHT_Y, w, LIGHT_H, light);
            shown_light = light;
        }

        let gear_mask = if st.gear == 0 {
            GLYPH_N
        } else {
            DIGITS[st.gear]
        };
        let gear_colour = if st.stalled { RED } else { WHITE };
        if gear_mask != shown_gear_mask {
            draw_seg_diff(
                GEAR_X,
                GEAR_Y,
                GEAR_W,
                GEAR_H,
                GEAR_T,
                shown_gear_mask,
                gear_mask,
                gear_colour,
            );
            shown_gear_mask = gear_mask;
        }

        let kmh = (st.speed * 36 / 10_000).min(999) as u32;
        let want = [
            if kmh >= 100 {
                DIGITS[(kmh / 100) as usize % 10]
            } else {
                GLYPH_BLANK
            },
            if kmh >= 10 {
                DIGITS[(kmh / 10) as usize % 10]
            } else {
                GLYPH_BLANK
            },
            DIGITS[(kmh % 10) as usize],
        ];
        for (i, m) in want.iter().enumerate() {
            if *m != shown_spd[i] {
                let x = SPD_X + i as u32 * (SPD_W + SPD_GAP);
                draw_seg_diff(x, SPD_Y, SPD_W, SPD_H, SPD_T, shown_spd[i], *m, GREEN);
                shown_spd[i] = *m;
            }
        }

        if missed_for > 0 {
            missed_for -= 1;
        }

        tick = tick.wrapping_add(1);
        if tick % 100 == 0 {
            let _ = writeln!(
                console,
                "manual_dash: gear={} rpm={} kmh={} thr={} up={} dn={}{}\r",
                if st.gear == 0 { 0 } else { st.gear },
                st.rpm,
                kmh,
                throttle,
                up_now as u8,
                down_now as u8,
                if st.stalled { " STALLED" } else { "" }
            );
        }

        let _ = Alarm::sleep_for(Milliseconds(TICK_MS));
    }
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

struct State {
    /// 0 is neutral, 1..=6 a gear.
    gear: usize,
    rpm: u32,
    /// Millimetres per second.
    speed: u32,
    stalled: bool,
}

impl State {
    fn step(&mut self, throttle: u32) {
        if self.stalled {
            // Dead engine. It still rolls, and it still slows down.
            self.rpm = 0;
            self.speed = apply_drag(self.speed, 0);
            if self.gear == 0 {
                self.stalled = false;
                self.rpm = IDLE_RPM;
            }
            return;
        }

        if self.gear == 0 {
            // Neutral: revs chase the throttle and fall away without it.
            let target = IDLE_RPM + (REDLINE_RPM - IDLE_RPM) * throttle / THROTTLE_FULL;
            self.rpm = if target > self.rpm {
                (self.rpm + NEUTRAL_RISE).min(target)
            } else {
                self.rpm
                    .saturating_sub(NEUTRAL_FALL)
                    .max(target.min(IDLE_RPM))
            };
            self.speed = apply_drag(self.speed, 0);
            return;
        }

        // In gear, the engine is bolted to the road: rpm follows speed.
        let couple = (RATIOS[self.gear] * FINAL_DRIVE) as u64;
        self.rpm = ((self.speed as u64 * couple) / RPM_DIVISOR) as u32;

        if self.rpm < STALL_RPM && self.speed < 1_200 {
            self.stalled = true;
            self.rpm = 0;
            return;
        }
        self.rpm = self.rpm.max(IDLE_RPM);

        // Over the limiter the spark cuts, so no drive at all until it falls
        // back under. That is what makes short-shifting worth doing.
        let drive = if self.rpm >= REDLINE_RPM {
            0
        } else {
            (torque(self.rpm) as u64 * throttle as u64 * couple) / (1_000 * 100)
        };

        self.speed = apply_drag(self.speed, drive);
    }
}

/// Torque, 0..=1000, over the rev range. Rises off idle, flat through the
/// middle, falls away at the top so there is a reason to change gear.
fn torque(rpm: u32) -> u32 {
    match rpm {
        0..=999 => 400 + (rpm * 300) / 1_000,
        1_000..=2_999 => 700 + ((rpm - 1_000) * 300) / 2_000,
        3_000..=5_499 => 1_000 - ((rpm - 3_000) * 100) / 2_500,
        5_500..=6_999 => 900 - ((rpm - 5_500) * 300) / 1_500,
        _ => 0,
    }
}

/// One tick of speed: net force over inertia.
///
/// **Drive and drag are both forces and both divide by `INERTIA`.** An earlier
/// version subtracted drag from speed directly, which made the two terms
/// different units -- first gear then reached the limiter in 20 ms. Caught by
/// simulating this exact arithmetic on the host before it went to the bench,
/// which is the only way to check a feel you cannot feel yet.
fn apply_drag(speed: u32, drive: u64) -> u32 {
    let s = speed as u64;
    let drag = (DRAG_LINEAR * s) / 1_000 + (DRAG_SQUARE * s * s) / 10_000_000;
    if drive >= drag {
        (s + ((drive - drag) * TICK_MS as u64) / INERTIA).min(u32::MAX as u64) as u32
    } else {
        s.saturating_sub(((drag - drive) * TICK_MS as u64) / INERTIA) as u32
    }
}

/// Stick above centre becomes throttle; at or below centre is closed.
fn throttle_from(raw: u32, centre: u32) -> u32 {
    if raw <= centre {
        return 0;
    }
    ((raw - centre) * THROTTLE_FULL / STICK_SPAN).min(THROTTLE_FULL)
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn seg_x(i: u32) -> u32 {
    TACH_X + i * (TACH_W + TACH_GAP)
}

fn tach_colour(i: u32) -> u16 {
    let rpm = i * REDLINE_RPM / TACH_SEGS;
    if rpm >= REDLINE_RPM - 400 {
        RED
    } else if rpm >= SHIFT_LIGHT_RPM {
        AMBER
    } else {
        GREEN
    }
}

/// Repaints only the segments that differ between `old` and `new`.
///
/// This is the whole reason the display is seven-segment rather than a font:
/// 88 -> 89 touches two rectangles. A glyph blit would move the entire cell
/// every time the last digit ticked.
#[allow(clippy::too_many_arguments)]
fn draw_seg_diff(x: u32, y: u32, w: u32, h: u32, t: u32, old: u8, new: u8, on: u16) {
    let changed = old ^ new;
    if changed == 0 {
        return;
    }
    let half = (h - 3 * t) / 2;
    for bit in 0..7u8 {
        if changed & (1 << bit) == 0 {
            continue;
        }
        let colour = if new & (1 << bit) != 0 { on } else { BG };
        let (sx, sy, sw, sh) = match bit {
            0 => (x + t, y, w - 2 * t, t),                    // A, top
            1 => (x + w - t, y + t, t, half),                 // B, upper right
            2 => (x + w - t, y + 2 * t + half, t, half),      // C, lower right
            3 => (x + t, y + 2 * t + 2 * half, w - 2 * t, t), // D, bottom
            4 => (x, y + 2 * t + half, t, half),              // E, lower left
            5 => (x, y + t, t, half),                         // F, upper left
            _ => (x + t, y + t + half, w - 2 * t, t),         // G, middle
        };
        rect(sx, sy, sw, sh, colour);
    }
}

/// One solid rectangle. Errors are ignored on purpose: a dropped frame on a
/// dashboard is a stale pixel, and stopping to report it would cost the next
/// one too. `screen_bench` is where drawing cost gets measured.
fn rect(x: u32, y: u32, w: u32, h: u32, colour: u16) {
    if w == 0 || h == 0 {
        return;
    }
    if Screen::set_write_frame(x, y, w, h).is_ok() {
        let mut pixel = [0u8; 2];
        let _ = Screen::fill(&mut pixel, colour);
    }
}
