//! Measures the frame path Doom will use, at Doom's frame size, without Doom.
//!
//! `doomgeneric` hands a platform six functions. Four of them are trivial on
//! Tock (`DG_Init`, `DG_SetWindowTitle`, `DG_GetTicksMs`, `DG_SleepMs` map onto
//! the alarm and the console). The two that decide whether the port is playable
//! are `DG_DrawFrame` and `DG_GetKey`, and this app is those two with a
//! synthetic renderer standing in for the game.
//!
//! # What it does
//!
//! Doom renders 320x200 8-bit paletted pixels. The panel takes RGB565. So every
//! frame costs a 256-entry lookup per pixel plus a blit, and this app pays both
//! at the real sizes: a 64,000-byte `u8` framebuffer, a 256-entry palette, and
//! a conversion into a staging buffer that goes out through `Screen::write`.
//!
//! The frame is centred on the 480x320 panel at (80, 60) rather than scaled.
//! Scaling is a second decision and it should be made against a measurement of
//! the unscaled cost, not at the same time.
//!
//! # Why bands
//!
//! A whole 320x200 frame is 128,000 bytes of RGB565, and writes do not continue
//! across syscalls -- `kit_screen_bars` measured that on the glass: forty
//! one-row writes all restarted at the frame origin and painted one row forty
//! times. So a frame is either one `write` of 128,000 bytes, or a `set_write_frame`
//! and a `write` per band.
//!
//! One write of the whole frame needs 128,000 bytes of staging on top of the
//! 64,000-byte framebuffer. Bands trade syscalls for memory, and the trade is
//! the thing being measured: **this app sweeps band heights and reports what
//! each one costs**, so the choice is made on numbers rather than on the
//! assumption that fewer syscalls must be faster.
//!
//! The comparison point is the 2.51 MB/s measured kernel-side for a single
//! large blit. If the smallest bands land near that, syscall overhead is not
//! the binding cost and the memory can go to Doom's heap instead.
//!
//! # What a reader should see
//!
//! An animated XOR pattern in the middle of the panel, on a black border. The
//! palette is four ramps -- grey, red, green, blue -- so a byte-order fault
//! shows up as the wrong ramp rather than as a subtle tint, and a stuck frame
//! shows up as a still picture.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::buttons::Buttons;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::platform::ErrorCode;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x20000}

/// Doom's own frame, not the panel's.
const DOOM_W: usize = 320;
const DOOM_H: usize = 200;

const PANEL_W: u32 = 480;
const PANEL_H: u32 = 320;

/// Centred, not scaled. See the module comment.
const ORIGIN_X: u32 = (PANEL_W - DOOM_W as u32) / 2;
const ORIGIN_Y: u32 = (PANEL_H - DOOM_H as u32) / 2;

/// The largest band this app stages. Raising it costs RAM that Doom's zone
/// heap needs more than this app does.
const MAX_BAND: usize = 50;
const STAGE_MAX: usize = DOOM_W * MAX_BAND * 2;

/// Swept largest to smallest. Each divides 200 exactly, so no trial has a
/// short final band whose cost would not be comparable with the others.
const BANDS: [usize; 4] = [50, 25, 10, 5];
const FRAMES_PER_TRIAL: u32 = 10;

/// The panel spends upwards of a second in its init sequence and refuses
/// everything with BUSY until it is done. Measured at 1125 ms from userspace.
const READY_POLL_MS: u32 = 25;
const READY_BUDGET_MS: u32 = 5_000;

fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

/// Four ramps, so a channel fault names itself. A real Doom palette comes out
/// of the WAD's PLAYPAL lump and costs exactly the same to look up.
fn build_palette(palette: &mut [u16; 256]) {
    for i in 0..256usize {
        let v = ((i % 64) * 4) as u8;
        palette[i] = match i / 64 {
            0 => rgb565(v, v, v),
            1 => rgb565(v, 0, 0),
            2 => rgb565(0, v, 0),
            _ => rgb565(0, 0, v),
        };
    }
}

/// Stands in for the renderer. Cheap on purpose: this app is measuring the
/// frame path, and a costly pattern would hide what it is measuring.
fn render(fb: &mut [u8], t: u8) {
    for y in 0..DOOM_H {
        let row = y * DOOM_W;
        for x in 0..DOOM_W {
            fb[row + x] = ((x ^ y) as u8).wrapping_add(t);
        }
    }
}

/// The inner loop of `DG_DrawFrame`: one palette lookup per pixel, high byte
/// first, which is the order `hil::screen` documents and the ST7796 takes.
fn convert(fb: &[u8], palette: &[u16; 256], stage: &mut [u8]) {
    for (i, &idx) in fb.iter().enumerate() {
        let c = palette[idx as usize];
        stage[i * 2] = (c >> 8) as u8;
        stage[i * 2 + 1] = c as u8;
    }
}

fn wait_ready<W: Write>(console: &mut W) -> bool {
    let mut waited = 0;
    loop {
        match Screen::set_write_frame(0, 0, PANEL_W, PANEL_H) {
            Ok(()) => {
                let _ = writeln!(console, "kit_doom_frame: panel ready after {waited} ms");
                return true;
            }
            Err(ErrorCode::Busy) if waited < READY_BUDGET_MS => {
                let _ = Alarm::sleep_for(libtock::alarm::Milliseconds(READY_POLL_MS));
                waited += READY_POLL_MS;
            }
            Err(e) => {
                let _ = writeln!(console, "kit_doom_frame: set_write_frame failed: {e:?}");
                return false;
            }
        }
    }
}

/// Paints the whole panel once so the border around Doom's frame is black
/// rather than whatever the last app left.
fn clear(stage: &mut [u8]) -> Result<(), ErrorCode> {
    let mut black = [0u8; 2];
    Screen::set_write_frame(0, 0, PANEL_W, PANEL_H)?;
    Screen::fill(&mut black, 0x0000)?;
    let _ = stage;
    Ok(())
}

fn main() {
    let mut console = Console::writer();

    if Screen::exists().is_err() {
        let _ = writeln!(console, "kit_doom_frame: no screen driver");
        return;
    }

    match Screen::get_resolution() {
        Ok((w, h)) => {
            let _ = writeln!(console, "kit_doom_frame: panel reports {w} x {h}");
            if w != PANEL_W || h != PANEL_H {
                let _ = writeln!(
                    console,
                    "  expected {PANEL_W} x {PANEL_H}; the frame is placed for that \
                     and will be clipped or offset here"
                );
            }
        }
        Err(e) => {
            let _ = writeln!(console, "kit_doom_frame: get_resolution failed: {e:?}");
            return;
        }
    }

    if !wait_ready(&mut console) {
        return;
    }

    let mut palette = [0u16; 256];
    build_palette(&mut palette);

    let mut fb = [0u8; DOOM_W * DOOM_H];
    let mut stage = [0u8; STAGE_MAX];

    let _ = clear(&mut stage);

    let _ = writeln!(
        console,
        "kit_doom_frame: {DOOM_W}x{DOOM_H} paletted, centred at ({ORIGIN_X}, {ORIGIN_Y})\n  \
         {} bytes framebuffer, {} bytes staging, {FRAMES_PER_TRIAL} frames per trial",
        fb.len(),
        stage.len()
    );
    let _ = writeln!(
        console,
        "  band  convert_ms  blit_ms  frame_ms   fps   blit_MB_s   syscalls/frame"
    );

    let mut t: u8 = 0;

    for &band in BANDS.iter() {
        let rows_per_band = band;
        let band_bytes = DOOM_W * rows_per_band * 2;
        let bands_per_frame = DOOM_H / rows_per_band;

        let mut convert_ms: u64 = 0;
        let mut blit_ms: u64 = 0;
        let frame_start = Alarm::get_milliseconds().unwrap_or(0);

        for _ in 0..FRAMES_PER_TRIAL {
            render(&mut fb, t);
            t = t.wrapping_add(3);

            for b in 0..bands_per_frame {
                let first_row = b * rows_per_band;
                let src = &fb[first_row * DOOM_W..(first_row + rows_per_band) * DOOM_W];

                let c0 = Alarm::get_milliseconds().unwrap_or(0);
                convert(src, &palette, &mut stage[..band_bytes]);
                let c1 = Alarm::get_milliseconds().unwrap_or(0);
                convert_ms += c1.saturating_sub(c0);

                let ok = Screen::set_write_frame(
                    ORIGIN_X,
                    ORIGIN_Y + first_row as u32,
                    DOOM_W as u32,
                    rows_per_band as u32,
                )
                .and_then(|()| Screen::write(&stage[..band_bytes]));
                let c2 = Alarm::get_milliseconds().unwrap_or(0);
                blit_ms += c2.saturating_sub(c1);

                if let Err(e) = ok {
                    let _ = writeln!(console, "  band {band}: write failed: {e:?}");
                    return;
                }
            }
        }

        let elapsed = Alarm::get_milliseconds()
            .unwrap_or(0)
            .saturating_sub(frame_start);
        let frames = FRAMES_PER_TRIAL as u64;
        let frame_ms = elapsed / frames;
        let fps = if frame_ms > 0 { 1000 / frame_ms } else { 0 };
        // Bytes blitted per trial, over the time spent in write calls.
        let blit_bytes = (DOOM_W * DOOM_H * 2) as u64 * frames;
        let kb_s = if blit_ms > 0 {
            blit_bytes / blit_ms // bytes per ms == kB/s
        } else {
            0
        };

        let _ = writeln!(
            console,
            "  {band:>4}  {:>10}  {:>7}  {:>8}  {:>4}  {:>7}.{:02}   {}",
            convert_ms / frames,
            blit_ms / frames,
            frame_ms,
            fps,
            kb_s / 1000,
            (kb_s % 1000) / 10,
            bands_per_frame * 2
        );
    }

    // DG_GetKey's other half: the kit's buttons are the only input this board
    // has, and four of them is enough for forward/back/turn.
    match Buttons::count() {
        Ok(n) => {
            let mut pressed = 0;
            for b in 0..n {
                if Buttons::is_pressed(b) {
                    pressed += 1;
                }
            }
            let _ = writeln!(
                console,
                "kit_doom_frame: {n} buttons, {pressed} held at exit -- DG_GetKey has an input"
            );
        }
        Err(e) => {
            let _ = writeln!(console, "kit_doom_frame: no buttons: {e:?}");
        }
    }

    let _ = writeln!(
        console,
        "kit_doom_frame: done. The picture left on the panel is the last frame \
         of the smallest band, so a still image here is the expected end state."
    );
}
