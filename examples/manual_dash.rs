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
use libtock::platform::ErrorCode;
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
/// Constant rolling resistance, the term that actually brings the car to rest.
/// Without it the drag force at low speed divided by inertia truncated to zero
/// and the car coasted forever -- 30 km/h to a standstill takes 59 s with it,
/// which is about what a real car in neutral does.
const DRAG_ROLL: u64 = 22_000;
/// Where the shift light comes on.
const SHIFT_LIGHT_RPM: u32 = 6_200;

/// How long the loop sleeps after each pass. **Not the timestep** — see below.
const TICK_MS: u32 = 10;
/// Clamp on measured dt, so a stall in the console or a very slow frame cannot
/// launch the car across the map in one step.
const DT_MIN_MS: u32 = 1;
const DT_MAX_MS: u32 = 60;

/// Drag. Linear term is speed-dependent losses, square term is aero. Both are
/// FORCES, in the same units as drive -- see `State::integrate`.
const DRAG_LINEAR: u64 = 90;
const DRAG_SQUARE: u64 = 440;
/// Bigger is heavier: divides NET force before it becomes acceleration.
/// Tuned against a host simulation of this exact integer arithmetic: first
/// gear 0-40 km/h in 1.9 s, 0-100 in 8.3 s, 202 km/h terminal in sixth.
const INERTIA: u64 = 165_000;

/// Engine inertia in neutral, **rpm per second**, and deliberately asymmetric:
/// a small four blips up faster than it falls, because combustion torque
/// against the flywheel is stronger than engine braking. Getting that backwards
/// reads as wrong to someone who cannot say why.
///
/// 800 -> 7000 in ~0.65 s, 7000 -> 800 in ~1.24 s. The previous values were a
/// rate per tick rather than per second and swept the whole range in 148 ms,
/// which is where "the bars advance and retreat too fast" came from -- the
/// engine had a rate limit, it was just four times too quick to read as a
/// gauge.
///
/// **In gear there is no equivalent and there should not be**: rpm is tied to
/// road speed, so the inertia is already carried by vehicle mass.
const REV_RISE_PER_S: u32 = 9_500;
const REV_FALL_PER_S: u32 = 5_000;

/// Throttle above which the clutch is out. Below it the clutch is IN -- which
/// is the whole of Jon's control scheme, throttle-release standing in for a
/// pedal. Sits above the stick's resting noise, measured at 0-2.
const CLUTCH_THROTTLE: u32 = 20;

/// How fast the engine converges on road speed as the clutch bites, rpm per
/// second. **A clutch takes time to engage, and skipping that is what made the
/// tach jump.** Assigning road speed to the engine directly moves the jump
/// around -- from gear selection to throttle application -- rather than
/// removing it. 15,000/s closes a 3,000 rpm gap in about 200 ms, which reads as
/// a firm bite; the fastest the engine is ever dragged by the road in normal
/// driving is about 3,300/s in first, so once engaged this tracks exactly.
const CLUTCH_BITE_PER_S: u32 = 15_000;

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

/// Tach bar across the top. **Continuous, not discrete blocks** -- see
/// `tach_span` for why the measurement chose that.
const TACH_X: u32 = 10;
const TACH_Y: u32 = 12;
const TACH_H: u32 = 44;
/// 10..470 on a 480-wide panel. An earlier 16x(28+2) layout ran to x=486 and
/// off the right edge; arithmetic, not eyesight, caught it.
const TACH_W: u32 = 460;
/// Quantisation. 46 steps of 10 px, about 152 rpm each -- fine enough to read
/// as movement, coarse enough that most ticks redraw nothing at all.
const TACH_STEP: u32 = 10;
/// Band edges as offsets into the bar.
const BAND_AMBER: u32 = TACH_W * SHIFT_LIGHT_RPM / REDLINE_RPM;
const BAND_RED: u32 = TACH_W * (REDLINE_RPM - 400) / REDLINE_RPM;

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
    let mut paint = Paint {
        w,
        h,
        ..Default::default()
    };
    paint.rect(0, 0, w, h, BG);

    let _ = writeln!(
        console,
        "manual_dash: {w}x{h}, stick centred at {centre} on ch{THROTTLE_CH}\r\n\
         \x20 button {SHIFT_UP_BTN} (GP15, left) = UP, button {SHIFT_DOWN_BTN} \
         (GP14, right) = DOWN\r\n\
         \x20 a shift only takes below {SHIFT_LIFT}/{THROTTLE_FULL} throttle -- \
         ask with the power on\r\n\
         \x20 and it drops to neutral. Lift, shift, power.\r\n\
         \x20 both buttons together = reset to neutral at rest, strip goes blue.\r"
    );

    // Static furniture, drawn once.
    paint.rect(TACH_X, TACH_Y, TACH_W, TACH_H, DIM);

    let mut st = State {
        gear: 0,
        rpm: IDLE_RPM,
        speed: 0,
        rem: 0,
    };

    // Everything the display is currently showing, so only differences are
    // drawn. Deliberately impossible starting values so the first frame paints.
    // What the glass is ACTUALLY showing, which after the clear above is
    // nothing lit. These were sentinels -- u32::MAX and 0xFF -- meant to force
    // a full first paint, and with an XOR diff that is exactly backwards: a
    // segment the sentinel claims is already on is never drawn, so five of the
    // gear character's seven and six of the speed zero's were never painted and
    // every later diff was computed against a lie. The sentinel also asked for
    // a rectangle 4,294,967,245 pixels wide.
    //
    // There was nothing to force. The screen is in a known state because this
    // app just cleared it, so the honest initial value is the true one.
    let mut shown_segs: u32 = 0;
    let mut shown_gear_mask: u8 = 0x00;
    let mut shown_spd: [u8; 3] = [0x00; 3];
    let mut shown_light: u16 = BG;

    let mut up_was = false;
    let mut down_was = false;
    let mut missed_for = 0u32;
    // Accepted shifts per button and refusals, monotonic. A 1 Hz sample of
    // `up=`/`dn=` cannot catch a press between prints -- every capture so far
    // has shown both zero, which says nothing. Counts survive the gap, and
    // separating the two buttons answers which one the driver means by "left"
    // without asking him to describe it again.
    let mut ups = 0u32;
    let mut dns = 0u32;
    let mut refused = 0u32;
    let mut tick: u32 = 0;

    let hz = Alarm::get_frequency()
        .map(|f| f.0)
        .unwrap_or(1_000_000)
        .max(1);
    let mut last = Alarm::get_ticks().unwrap_or(0);

    loop {
        // MEASURED timestep, not the sleep length. `sleep_for` waits TICK_MS
        // AFTER the work, so the real period is work + TICK_MS -- about 14.6 ms
        // at a typical draw cost of four operations, not the 10 ms the
        // drivetrain was tuned against. Feeding the tuned constants a fixed 10
        // would make the car accelerate about a third slower than the host
        // simulation said it should, and the discrepancy would read as "the
        // torque curve is wrong".
        let now = Alarm::get_ticks().unwrap_or(last);
        let dt = ((now.wrapping_sub(last) as u64 * 1_000) / hz as u64) as u32;
        let dt = dt.clamp(DT_MIN_MS, DT_MAX_MS);
        last = now;

        let raw = Adc::read_single_sample_sync(THROTTLE_CH).unwrap_or(centre as u16) as u32;
        let throttle = throttle_from(raw, centre);

        let up_now = Buttons::is_pressed(SHIFT_UP_BTN);
        let down_now = Buttons::is_pressed(SHIFT_DOWN_BTN);
        let want_up = up_now && !up_was;
        let want_down = down_now && !down_was;
        up_was = up_now;
        down_was = down_now;

        // BOTH BUTTONS: back to a known state. Any state the driver cannot
        // leave using the controls on the board is a dead end -- when the
        // gearbox last trapped itself, the board could not say so and a human
        // had to. This costs four lines and removes that class.
        let both_reset = up_now && down_now;
        if both_reset {
            st = State {
                gear: 0,
                rpm: IDLE_RPM,
                speed: 0,
                rem: 0,
            };
            missed_for = 0;
        } else if want_up || want_down {
            if throttle >= SHIFT_LIFT {
                // REFUSED: the box will not take it with the power on. The gear
                // does not change.
                //
                // It used to drop to neutral, chosen for legibility back when
                // the car could not move at all. Now that it can, ejecting the
                // driver from gear at 28 km/h leaves no way back in without
                // almost stopping -- a punishment out of all proportion to
                // lifting a fraction too late, and one that reads as a bug even
                // when the logic is right. The cost of a missed shift is
                // already real without it: you do not get the gear, and the
                // engine keeps climbing toward the limiter while you try again.
                refused = refused.saturating_add(1);
                missed_for = 60;
            } else if want_up {
                if st.gear < TOP_GEAR {
                    st.gear += 1;
                }
                ups = ups.saturating_add(1);
            } else {
                if st.gear > 0 {
                    st.gear -= 1;
                }
                dns = dns.saturating_add(1);
            }
        }

        st.step(throttle, dt);

        // ---- display, differences only --------------------------------------

        let lit = tach_width(st.rpm);
        if lit != shown_segs {
            if lit > shown_segs {
                tach_span(&mut paint, shown_segs, lit, true);
            } else {
                tach_span(&mut paint, lit, shown_segs, false);
            }
            shown_segs = lit;
        }

        let light = if both_reset {
            // Visible confirmation on the glass, not just in a transcript --
            // the driver holding both buttons is the one person who cannot
            // read the console.
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
            paint.rect(0, LIGHT_Y, w, LIGHT_H, light);
            shown_light = light;
        }

        let gear_mask = if st.gear == 0 {
            GLYPH_N
        } else {
            DIGITS[st.gear]
        };
        let gear_colour = WHITE;
        if gear_mask != shown_gear_mask {
            draw_seg_diff(
                &mut paint,
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
                draw_seg_diff(
                    &mut paint,
                    x,
                    SPD_Y,
                    SPD_W,
                    SPD_H,
                    SPD_T,
                    shown_spd[i],
                    *m,
                    GREEN,
                );
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
                "manual_dash: gear={} rpm={} kmh={} thr={} dt={}ms up={} dn={} \
                 ups={} dns={} ref={} draws={} err={} clip={} last={:?}{}\r",
                if st.gear == 0 { 0 } else { st.gear },
                st.rpm,
                kmh,
                throttle,
                dt,
                up_now as u8,
                down_now as u8,
                ups,
                dns,
                refused,
                paint.calls,
                paint.errs,
                paint.clipped,
                paint.last,
                if both_reset { " RESET" } else { "" }
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
    /// Sub-millimetre remainder carried between ticks.
    ///
    /// **Without it every coast below about 70 km/h truncated to zero.** The
    /// drag force divided by inertia came to less than 1 mm/s per tick, integer
    /// division threw it away, and the car held its speed forever -- which on
    /// the glass read as a frozen dashboard.
    rem: i64,
}

impl State {
    fn step(&mut self, throttle: u32, dt: u32) {
        if self.gear == 0 {
            // Neutral: revs chase the throttle and fall away without it.
            self.rpm = free_rev(self.rpm, throttle, dt);
            self.integrate(0, dt);
            return;
        }

        // In gear. The engine is bolted to the road ONLY once the road is
        // turning it fast enough to idle -- below that the clutch has to slip,
        // and without that there is no way to start moving at all.
        let couple = (RATIOS[self.gear] * FINAL_DRIVE) as u64;
        let implied = ((self.speed as u64 * couple) / RPM_DIVISOR) as u32;

        if throttle < CLUTCH_THROTTLE {
            // CLUTCH IN. Lifting off is the clutch pedal, so a released
            // throttle disengages: the engine returns to idle and the car
            // coasts on drag. Selecting a gear while rolling is then a no-op
            // until the throttle is fed, which is how a clutch actually works
            // -- and the alternative, locking on road speed alone, snapped the
            // tach to 3,500 rpm the instant first was selected at 20 km/h.
            //
            // The cost, taken deliberately: there is no engine braking. One
            // input is doing two jobs and coherence beats completeness. If it
            // is missed, the answer is a partial engagement on a trailing
            // throttle, not locking on speed again.
            self.rpm = free_rev(self.rpm, throttle, dt);
        } else if implied < IDLE_RPM {
            // SLIPPING. Below lock speed the engine revs to the throttle as in
            // neutral, and its torque still reaches the wheels -- which is what
            // lets the car pull away at all.
            self.rpm = free_rev(self.rpm.max(IDLE_RPM), throttle, dt);
        } else {
            // BITING, then locked. Converge on road speed rather than
            // assuming it: a clutch takes time, and an instant assignment is
            // a jump wherever it happens to land.
            self.rpm = slew(self.rpm, implied, CLUTCH_BITE_PER_S, dt);
        }

        // Over the limiter the spark cuts, so no drive at all until it falls
        // back under. That is what makes short-shifting worth doing.
        let drive = if self.rpm >= REDLINE_RPM {
            0
        } else {
            (torque(self.rpm) as u64 * throttle as u64 * couple) / (1_000 * 100)
        };

        self.integrate(drive, dt);
    }
}

/// One tick of an engine that is not tied to the road: revs chase the throttle
/// at the inertia rates, asymmetrically. Used in neutral and while the clutch
/// slips, which are the same situation mechanically.
fn free_rev(rpm: u32, throttle: u32, dt: u32) -> u32 {
    let target = IDLE_RPM + (REDLINE_RPM - IDLE_RPM) * throttle / THROTTLE_FULL;
    let rate = if target > rpm {
        REV_RISE_PER_S
    } else {
        REV_FALL_PER_S
    };
    slew(rpm, target, rate, dt)
}

/// Move `rpm` toward `target` at `per_s`, without overshooting.
fn slew(rpm: u32, target: u32, per_s: u32, dt: u32) -> u32 {
    let step = per_s * dt / 1_000;
    if target > rpm {
        (rpm + step).min(target)
    } else {
        rpm.saturating_sub(step).max(target)
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

/// One tick of speed: net force over inertia, carrying the remainder.
///
/// **Drive and drag are both forces and both divide by `INERTIA`.** An earlier
/// version subtracted drag from speed directly, which made the two terms
/// different units -- first gear then reached the limiter in 20 ms.
///
/// The carry is the second fix and a separate bug: at these constants a coast
/// below roughly 70 km/h produced less than 1 mm/s of change per tick, integer
/// division discarded it, and the car held its speed indefinitely. Keeping the
/// remainder makes arbitrarily small forces integrate correctly over time,
/// which is what a fixed-point integrator has to do to be one at all.
impl State {
    fn integrate(&mut self, drive: u64, dt: u32) {
        let s = self.speed as i64;
        let drag = (DRAG_ROLL
            + (DRAG_LINEAR * s as u64) / 1_000
            + (DRAG_SQUARE * (s * s) as u64) / 10_000_000) as i64;
        let num = (drive as i64 - drag) * dt as i64 + self.rem;
        let inertia = INERTIA as i64;
        let delta = num.div_euclid(inertia);
        self.rem = num.rem_euclid(inertia);
        self.speed = (s + delta).max(0) as u32;
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

/// Lit width in pixels for an rpm, quantised so most ticks change nothing.
fn tach_width(rpm: u32) -> u32 {
    let w = rpm.min(REDLINE_RPM) * TACH_W / REDLINE_RPM;
    (w / TACH_STEP) * TACH_STEP
}

/// Paints `[from, to)` of the bar, lit in its band colours or unlit in `DIM`.
///
/// **This is where the panel benchmark changed the design.** Drawing cost is a
/// fixed ~1.16 ms per operation plus ~0.338 us per pixel, so at gauge sizes the
/// FIXED term dominates: a 30x30 block is about 80% overhead. Discrete segments
/// redrawn one at a time therefore cost n x 1.46 ms, and an upshift moves the
/// needle six or seven of them at once -- about 10 ms, a third of a 30 fps
/// frame, for one gauge.
///
/// A continuous bar collapses that to **at most three fills** whatever the
/// jump, because a growing span crosses at most three colour bands and a
/// shrinking one is a single `DIM` fill. That is why the bar is continuous
/// rather than blocks: the alternative rule -- draw segments individually up to
/// four, then repaint the whole bar -- cannot be done in one call, since a
/// `fill` paints one colour and the bar has three.
fn tach_span(p: &mut Paint, from: u32, to: u32, lit: bool) {
    if to <= from {
        return;
    }
    if !lit {
        p.rect(TACH_X + from, TACH_Y, to - from, TACH_H, DIM);
        return;
    }
    for (lo, hi, colour) in [
        (0, BAND_AMBER, GREEN),
        (BAND_AMBER, BAND_RED, AMBER),
        (BAND_RED, TACH_W, RED),
    ] {
        let a = from.max(lo);
        let b = to.min(hi);
        if b > a {
            p.rect(TACH_X + a, TACH_Y, b - a, TACH_H, colour);
        }
    }
}

/// Repaints only the segments that differ between `old` and `new`.
///
/// This is the whole reason the display is seven-segment rather than a font:
/// 88 -> 89 touches two rectangles. A glyph blit would move the entire cell
/// every time the last digit ticked.
#[allow(clippy::too_many_arguments)]
fn draw_seg_diff(p: &mut Paint, x: u32, y: u32, w: u32, h: u32, t: u32, old: u8, new: u8, on: u16) {
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
        p.rect(sx, sy, sw, sh, colour);
    }
}

/// One solid rectangle. Errors are ignored on purpose: a dropped frame on a
/// dashboard is a stale pixel, and stopping to report it would cost the next
/// one too. `screen_bench` is where drawing cost gets measured.
/// Draw-call accounting.
///
/// A frozen display has at least three causes and they are indistinguishable
/// from the glass: the app stopped issuing draws, the capsule started refusing
/// them, or the driver stopped acting on them. **`calls` separates the first
/// from the other two and `errs`/`last` separate the second from the third**,
/// and none of that is recoverable after the fact — it has to be in the
/// transcript while it happens.
///
/// Note what a live telemetry line already rules out: a screen call that never
/// returns would block the loop in `yield_wait`, so if `dt` is still printing,
/// the app is not stuck inside a draw.
#[derive(Default)]
struct Paint {
    calls: u32,
    errs: u32,
    /// Rectangles refused here for falling outside the panel. A caller asking
    /// for one is a bug in the caller, and counting it separately keeps a
    /// programming error from hiding among real driver rejections -- the whole
    /// value of `errs` is that zero means something.
    clipped: u32,
    last: Option<ErrorCode>,
    w: u32,
    h: u32,
}

impl Paint {
    fn rect(&mut self, x: u32, y: u32, w: u32, h: u32, colour: u16) {
        if w == 0 || h == 0 {
            return;
        }
        if x.saturating_add(w) > self.w || y.saturating_add(h) > self.h {
            self.clipped += 1;
            return;
        }
        self.calls += 1;
        let r = match Screen::set_write_frame(x, y, w, h) {
            Ok(()) => {
                let mut pixel = [0u8; 2];
                Screen::fill(&mut pixel, colour)
            }
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            self.errs += 1;
            self.last = Some(e);
        }
    }
}
