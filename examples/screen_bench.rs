//! What the kit's panel actually costs per drawing operation.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=screen_bench
//! ```
//!
//! Built to settle a design fork rather than to admire numbers: a swept needle
//! redraws a large box every frame, a segmented ring redraws only the blocks
//! that changed, and the ratio between them decides which a tachometer should
//! be. The wire-time floor is derivable — 480x320 at 16 bpp over a 62.5 MHz SPI
//! is 39.32 ms — but the floor is not the budget. **The budget is the floor plus
//! the syscall path, the per-window command and the capsule's chunking**, and
//! that gap is what this measures.
//!
//! # The ready gap is absorbed before timing starts
//!
//! One throwaway operation runs first. Since tock `5c0185624` the capsule
//! queues a command the panel is not ready for and serves it at
//! `screen_is_ready`, so that first call simply blocks for the init gap —
//! about 1,239 ms — instead of failing. Timing anything before it would put
//! the whole gap into the first sample and nothing into the rest.
//!
//! # Why `write` is timed as well as `fill`
//!
//! They cost differently for reasons that have nothing to do with area.
//! `fill` sends **one pixel value** and the capsule replicates it into its own
//! buffer, so 2 bytes cross the syscall boundary whatever the rectangle.
//! `write` sends the caller's whole framebuffer across. For a gauge made of
//! solid blocks that difference may dominate everything else in the budget.
//!
//! # Why two `write` sizes and not five
//!
//! The capsule's buffer is 12,800 bytes — 6,400 px at RGB565 — and a larger
//! write is **chunked, not rejected**: `write_complete` re-fills the buffer and
//! re-issues with `continue_write` true until the caller's slice is exhausted.
//! So 6,400 px is exactly one chunk and 12,800 px is exactly two, and the
//! difference between them is one kernel round-trip with the wire time of the
//! second chunk. That isolates the per-chunk cost, which is the number to
//! budget against when a frame spans several chunks.
//!
//! # Reading it
//!
//! Min and median over `REPS` runs. Min is the clean case; a median far above
//! it means something else on the board is interfering, which on an otherwise
//! idle machine would itself be a finding. The alarm frequency is printed
//! because the tick is not obliged to be 1 MHz.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x8000}

/// Runs per size. Enough to see a spread without making the 480x320 row take
/// all day: at roughly 40 ms a full-screen fill, 16 of them is under a second.
const REPS: usize = 16;

/// Rectangles to time, widest last. The last is the whole panel.
const SIZES: [(u32, u32); 5] = [(16, 16), (64, 64), (120, 80), (240, 160), (480, 320)];

/// One chunk of the capsule's buffer, in pixels, and two of them. Bytes are
/// twice these.
const CHUNK_PX: usize = 6_400;
const WRITE_PX: [usize; 2] = [CHUNK_PX, CHUNK_PX * 2];

const BG: u16 = 0x0000;
const FG: u16 = 0x07E0;

fn main() {
    let mut console = Console::writer();

    if Screen::exists().is_err() {
        let _ = writeln!(
            console,
            "screen_bench: no screen driver at 0x90001. The kernel needs the \
             board feature that wires the panel to userspace.\r"
        );
        return;
    }

    let hz = match Alarm::get_frequency() {
        Ok(f) if f.0 > 0 => f.0,
        _ => {
            let _ = writeln!(console, "screen_bench: no usable alarm frequency\r");
            return;
        }
    };

    let (w, h) = match Screen::get_resolution() {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(console, "screen_bench: get_resolution: {e:?}\r");
            return;
        }
    };

    // Absorb the readiness gap. This call blocks for it; nothing after this
    // point pays for it. Timed anyway, because how long it took is worth
    // seeing and confirms the queue-at-ready path is what is happening.
    let t0 = Alarm::get_ticks().unwrap_or(0);
    let ready = fill_rect(0, 0, w, h, BG);
    let t1 = Alarm::get_ticks().unwrap_or(0);
    let _ = writeln!(
        console,
        "screen_bench: {w}x{h}, alarm {hz} Hz\r\n\
         \x20 first full-screen fill {ready:?} in {} us -- this one carries the \
         readiness gap\r\n\
         \x20 {REPS} reps per row, times are microseconds\r",
        us(t0, t1, hz)
    );
    if ready.is_err() {
        let _ = writeln!(
            console,
            "screen_bench: panel refused the first fill; stopping\r"
        );
        return;
    }

    let _ = writeln!(
        console,
        "screen_bench: FILL -- set_write_frame + fill, 2 bytes over the syscall \
         boundary\r\n\
         \x20      size      px      min   median\r"
    );
    for (rw, rh) in SIZES {
        if rw > w || rh > h {
            continue;
        }
        let mut t = [0u32; REPS];
        let mut errs = 0u32;
        for slot in t.iter_mut() {
            let a = Alarm::get_ticks().unwrap_or(0);
            let r = fill_rect(0, 0, rw, rh, if errs % 2 == 0 { FG } else { BG });
            let b = Alarm::get_ticks().unwrap_or(0);
            if r.is_err() {
                errs += 1;
            }
            *slot = us(a, b, hz);
        }
        sort(&mut t);
        let _ = writeln!(
            console,
            "  {:>4}x{:<4} {:>7}  {:>7} {:>7}{}\r",
            rw,
            rh,
            rw * rh,
            t[0],
            t[REPS / 2],
            if errs > 0 { "  ERRORS" } else { "" }
        );
    }

    // `write` moves the caller's bytes. One chunk and two, so the difference
    // is one kernel round-trip plus the second chunk's wire time.
    let mut buf = [0u8; CHUNK_PX * 2 * 2];
    for pair in buf.chunks_exact_mut(2) {
        pair.copy_from_slice(&FG.to_be_bytes());
    }
    let _ = writeln!(
        console,
        "screen_bench: WRITE -- set_write_frame + write, caller's bytes over the \
         boundary\r\n\
         \x20    px    bytes  chunks      min   median\r"
    );
    for px in WRITE_PX {
        // A rectangle of exactly this many pixels: full panel width, so the
        // row count is the only thing that changes.
        let rows = px as u32 / w;
        if rows == 0 || rows > h {
            continue;
        }
        let bytes = (w * rows) as usize * 2;
        let mut t = [0u32; REPS];
        let mut errs = 0u32;
        for slot in t.iter_mut() {
            let a = Alarm::get_ticks().unwrap_or(0);
            let r = write_rect(0, 0, w, rows, &buf[..bytes]);
            let b = Alarm::get_ticks().unwrap_or(0);
            if r.is_err() {
                errs += 1;
            }
            *slot = us(a, b, hz);
        }
        sort(&mut t);
        let _ = writeln!(
            console,
            "  {:>5} {:>8} {:>7}  {:>7} {:>7}{}\r",
            w * rows,
            bytes,
            bytes.div_ceil(CHUNK_PX * 2),
            t[0],
            t[REPS / 2],
            if errs > 0 { "  ERRORS" } else { "" }
        );
    }

    let _ = writeln!(
        console,
        "screen_bench: done. A 6,400 px fill near 1.6 ms means the wire \
         dominates and the\r\n\
         \x20 derived model holds; well above it means the syscall and chunk \
         path dominate,\r\n\
         \x20 and the drawing should get cheaper rather than more ambitious.\r"
    );
}

/// Ticks to microseconds across a counter that wraps at 2**32.
fn us(a: u32, b: u32, hz: u32) -> u32 {
    ((b.wrapping_sub(a) as u64 * 1_000_000) / hz as u64) as u32
}

fn fill_rect(
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    colour: u16,
) -> Result<(), libtock::platform::ErrorCode> {
    Screen::set_write_frame(x, y, w, h)?;
    let mut pixel = [0u8; 2];
    Screen::fill(&mut pixel, colour)
}

fn write_rect(
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    pixels: &[u8],
) -> Result<(), libtock::platform::ErrorCode> {
    Screen::set_write_frame(x, y, w, h)?;
    Screen::write(pixels)
}

/// Insertion sort. `REPS` is small and this avoids pulling in anything.
fn sort(v: &mut [u32]) {
    for i in 1..v.len() {
        let mut j = i;
        while j > 0 && v[j - 1] > v[j] {
            v.swap(j - 1, j);
            j -= 1;
        }
    }
}
