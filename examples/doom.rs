//! Doom, on the kit's ST7796.
//!
//! The C is `doomgeneric` compiled for this target with clang and the libc
//! shim in that tree's `tock/`; this file is the other side of two contracts.
//!
//! **doomgeneric's six hooks.** `DG_Init`, `DG_DrawFrame`, `DG_SleepMs`,
//! `DG_GetTicksMs`, `DG_GetKey`, `DG_SetWindowTitle`. They are `#[no_mangle]
//! extern "C"` here rather than in a `doomgeneric_tock.c`, so the screen and
//! button drivers stay on the Rust side where the rest of the port measured
//! them.
//!
//! **The shim's five.** `tock_console_write`, `tock_ticks_ms`, `tock_exit`,
//! and the heap and WAD it is pointed at. Keeping that list short was the
//! point of sizing the shim by what Doom actually calls: the whole surface
//! between the two languages is eleven symbols and `nm` can check it.
//!
//! # Memory
//!
//! A process here gets 392 KiB, and every number below is spent against that.
//! `HEAP_BYTES` is what the shim's allocator hands out and is almost entirely
//! Doom's zone, which takes it in one call; the zone size is passed as `-kb`
//! so the two cannot silently disagree. `DG_ScreenBuffer` costs nothing
//! because `I_InitGraphics` aliases it onto `I_VideoBuffer` -- the frame is
//! 8-bit paletted on both sides, so a second buffer would only hold a copy.
//!
//! # The frame path
//!
//! Measured before Doom existed here, by `kit_doom_frame`: 320x200 8-bit
//! paletted out of Doom, a 256-entry lookup per pixel into RGB565, and a blit
//! per band because a screen write does not continue across syscalls. The
//! panel is 480x320 and the frame is centred rather than scaled.

#![no_main]
#![no_std]

use core::fmt::Write;
use core::ptr::addr_of_mut;
use core::cell::Cell;
use libtock::adc::{ADCListener, Adc};
use libtock::platform::{Syscalls, share};
use libtock::runtime::TockSyscalls;
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buttons::Buttons;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}

/// Doom recurses through the BSP, so this is not a formality. Not yet measured
/// -- 32 KiB is a guess with room in it, and the first thing to shrink if the
/// zone needs more.
stack_size! {0x1000}

const DOOM_W: usize = 320;
const DOOM_H: usize = 200;
const PANEL_W: u32 = 480;
const PANEL_H: u32 = 320;
const ORIGIN_X: u32 = (PANEL_W - DOOM_W as u32) / 2;
const ORIGIN_Y: u32 = (PANEL_H - DOOM_H as u32) / 2;

/// Rows per blit. 50 was the fastest band `kit_doom_frame` measured and costs
/// 32,000 bytes of staging; it divides 200 exactly, so no band is short.
const BAND: usize = 10;
const STAGE_BYTES: usize = DOOM_W * BAND * 2;

/// What the shim's allocator hands out. Doom's zone takes nearly all of it in
/// one call, so this and `ZONE_KIB` move together.
const HEAP_BYTES: usize = 268 * 1024;
/// Passed to Doom as `-kb`. Leaves the heap a few KiB for the handful of
/// strings Doom duplicates outside the zone.
const ZONE_KIB: usize = 236;

/// The panel spends over a second in its init sequence and answers BUSY until
/// it is done. Measured at 1225 ms from userspace.
const READY_POLL_MS: u32 = 25;
const READY_BUDGET_MS: u32 = 5_000;

// Doom's own, from doomkeys.h -- all six looked up there, none recalled.
//
// doomgeneric does NOT use vanilla Doom's bindings for these two. It defines
// dedicated codes, and m_controls.c's `key_fire = KEY_FIRE` resolves to them:
//
//     #define KEY_USE   0xa2
//     #define KEY_FIRE  0xa3
//
// Filling in KEY_RCTRL (0x9d) and space from what vanilla Doom uses got the
// buttons silently ignored while the arrows, which were looked up, worked.
const KEY_FIRE: u8 = 0xa3;
const KEY_USE: u8 = 0xa2;
const KEY_UP: u8 = 0xad;
const KEY_DOWN: u8 = 0xaf;
const KEY_LEFT: u8 = 0xac;
const KEY_RIGHT: u8 = 0xae;

/// The six things this board can ask Doom to do, in the order DG_GetKey
/// reports them.
const KEYS: [u8; 6] = [KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_FIRE, KEY_USE];

/// Joystick axes. GP26 is ADC0 and GP27 is ADC1, both confirmed rail to rail.
///
/// THE STICK IS MOUNTED 90 DEGREES CLOCKWISE on this kit, reported from the
/// bench: pushing it right read as forward. So channel 0 is the HORIZONTAL
/// axis here and channel 1 is the vertical one, not the other way round.
///
/// That also settles an apparent contradiction in the pin map, which records
/// "held the stick down, GP26 went low". Both are true: the board was held a
/// quarter turn differently when it was probed, so that "down" and this
/// "left" are the same physical direction.
const AXIS_MOVE: u32 = 1;
const AXIS_TURN: u32 = 0;

/// The stick rests near mid-scale and the converter is 16-bit. A quarter of
/// full travel either way is a deadzone wide enough that a stick which does
/// not self-centre perfectly will not walk the player into a wall on its own.
const AXIS_LOW: u16 = 16_384;
const AXIS_HIGH: u16 = 49_152;

/// Set if an axis runs the other way.
///
/// Turn is settled: right reads high, which is what the bench saw. Which sign
/// of the vertical axis is "forward" is NOT settled -- a quarter turn swaps
/// the axes and may or may not flip one, and nothing has measured which. The
/// app prints both raw values, so the next run with the stick held forward
/// answers it rather than a coin toss.
const INVERT_MOVE: bool = false;
const INVERT_TURN: bool = false;

static WAD: &[u8] = include_bytes!("assets/wad_trim.bin");
/// The name `fopen` answers to. Doom's IWAD search wants a name that exists;
/// on this device exactly one does, and this is it.
static WAD_NAME: &[u8] = b"doom.wad\0";

// ---------------------------------------------------------------- the shim's

static mut HEAP: [u8; HEAP_BYTES] = [0; HEAP_BYTES];

#[no_mangle]
pub static mut tock_heap_base: *mut u8 = core::ptr::null_mut();
#[no_mangle]
pub static mut tock_heap_size: usize = 0;
#[no_mangle]
pub static mut tock_wad_base: *const u8 = core::ptr::null();
#[no_mangle]
pub static mut tock_wad_length: usize = 0;
#[no_mangle]
pub static mut tock_wad_name: *const u8 = core::ptr::null();

#[no_mangle]
pub extern "C" fn tock_console_write(buf: *const u8, len: usize) {
    if buf.is_null() || len == 0 {
        return;
    }
    // SAFETY: the shim passes a pointer to `len` initialised bytes it owns for
    // the duration of the call, and Console::write only reads them.
    let bytes = unsafe { core::slice::from_raw_parts(buf, len) };
    let _ = Console::write(bytes);
}

#[no_mangle]
pub extern "C" fn tock_ticks_ms() -> u32 {
    Alarm::get_milliseconds().unwrap_or(0) as u32
}

/// The shim's own accounting, which nothing has read back until now.
extern "C" {
    fn tock_alloc_stats(used: *mut usize, peak: *mut usize,
                        calls: *mut u32, dropped: *mut u32);
}

/// Deepest the stack has been, by painting it and looking for the paint.
///
/// `stack_size!` puts the stack in STACK_MEMORY and it grows DOWN from the
/// end, so unused space is at the START of the array. Painting stops short of
/// the live frame; anything below the first surviving byte was reached.
const PAINT: u8 = 0xAA;

fn paint_stack() {
    let base = addr_of_mut!(STACK_MEMORY) as usize;
    let here = &base as *const _ as usize;
    // Leave a wide margin below the current frame: this function's own
    // locals, and everything it returns into, live above it.
    let safe = here.saturating_sub(base).saturating_sub(512);
    // SAFETY: writing only below the live frame, in the app's own stack array.
    unsafe {
        let p = addr_of_mut!(STACK_MEMORY) as *mut u8;
        for i in 0..safe {
            p.add(i).write_volatile(PAINT);
        }
    }
}

fn stack_used() -> (usize, usize) {
    let total = unsafe { (*addr_of_mut!(STACK_MEMORY)).len() };
    // SAFETY: reading the app's own stack array.
    let p = addr_of_mut!(STACK_MEMORY) as *const u8;
    let mut untouched = 0;
    while untouched < total && unsafe { p.add(untouched).read_volatile() } == PAINT {
        untouched += 1;
    }
    (total - untouched, total)
}

fn report_budget<W: Write>(console: &mut W) {
    // SAFETY: single-threaded read of values the input path writes.
    let (raw, lo, hi) = unsafe { (AXIS_RAW, AXIS_MIN, AXIS_MAX) };
    let _ = writeln!(
        console,
        "doom: stick move={} [{}..{}]  turn={} [{}..{}]  (ch1 vertical, ch0 horizontal)",
        raw[0], lo[0], hi[0], raw[1], lo[1], hi[1]
    );
    // SAFETY: single-threaded read of values the input path writes.
    let (presses, errs) = unsafe { (BTN_PRESSES, BTN_ERRORS) };
    let _ = writeln!(
        console,
        "doom: buttons GP14={} GP15={} presses, {} driver errors",
        presses[0], presses[1], errs
    );
    // SAFETY: single-threaded read of values the input path writes.
    let (aok, aerr, alast) = unsafe { (ADC_OK, ADC_ERRORS, ADC_LAST_ERR) };
    let _ = writeln!(
        console,
        "doom: adc ch1 {} ok / {} err (last {}), ch0 {} ok / {} err (last {})",
        aok[0], aerr[0], alast[0], aok[1], aerr[1], alast[1]
    );
    let (used, total) = stack_used();
    let (mut hused, mut hpeak) = (0usize, 0usize);
    let (mut calls, mut dropped) = (0u32, 0u32);
    // SAFETY: four out-pointers to locals, which is what the shim expects.
    unsafe { tock_alloc_stats(&mut hused, &mut hpeak, &mut calls, &mut dropped) }
    let _ = writeln!(console,
        "doom: stack {used} of {total} used; heap peak {hpeak} of {HEAP_BYTES} \
         in {calls} calls, {dropped} frees dropped");
}

#[no_mangle]
pub extern "C" fn tock_exit(status: i32) -> ! {
    let mut console = Console::writer();
    // SAFETY: a NUL-terminated C string in the Doom image, written only by
    // I_Error before it calls exit.
    let why = unsafe {
        let p = addr_of_mut!(dg_last_error) as *const u8;
        let mut n = 0usize;
        while n < 512 && *p.add(n) != 0 {
            n += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or("(not utf-8)")
    };
    // Say it repeatedly, not once. Printed once on a serial console nobody is
    // watching, an error is indistinguishable from a freeze -- and that cost an
    // hour of hunting a hang that had already explained itself.
    loop {
        let _ = writeln!(console, "\ndoom: STOPPED -- exit({status})");
        if !why.is_empty() {
            let _ = writeln!(console, "doom: I_Error: {why}");
        }
        report_budget(&mut console);
        let _ = Alarm::sleep_for(Milliseconds(5000));
    }
}

// --------------------------------------------------------------- doomgeneric

extern "C" {
    fn doomgeneric_Create(argc: i32, argv: *mut *mut u8);
    fn doomgeneric_Tick();

    /// 8-bit paletted, `DOOM_W * DOOM_H`. Aliased onto `I_VideoBuffer`.
    static DG_ScreenBuffer: *mut u8;
    /// `struct color { b:8, g:8, r:8, a:8 }` packed in a u32, so byte 0 is
    /// blue. Non-static in `i_video.c` under CMAP256 precisely so a platform
    /// can read it.
    static colors: [u32; 256];
    /// `boolean`, which is `unsigned int` in doomtype.h.
    static mut palette_changed: u32;

    /// The last message I_Error was given; empty if it was never called.
    static mut dg_last_error: [u8; 512];
}

static mut PALETTE: [u16; 256] = [0; 256];
static mut STAGE: [u8; STAGE_BYTES] = [0; STAGE_BYTES];
/// What the inputs say, and what Doom has been told they say.
static mut WANT: [bool; 6] = [false; 6];
static mut TOLD: [bool; 6] = [false; 6];
static mut IN_BURST: bool = false;
/// Last reading per axis, and the extremes ever seen.
///
/// The extremes are the point. A timed console capture only answers if it
/// happens to overlap with someone holding the stick, which is a property of
/// the timing rather than of the hardware; a high-water and low-water mark
/// answers whenever it is read. `u16::MAX`/0 as the initial pair means
/// "nothing seen yet" reads as an impossible range rather than as centre.
static mut AXIS_RAW: [u16; 2] = [0; 2];
static mut AXIS_MIN: [u16; 2] = [u16::MAX; 2];
static mut AXIS_MAX: [u16; 2] = [0; 2];

/// Presses seen per button, and errors from the driver, since boot.
///
/// The same reason the axes keep watermarks: "the button does nothing" has
/// three different causes -- the driver never says pressed, the driver errors,
/// or Doom ignores the key -- and they look identical from the outside. A
/// count that only rises when the DRIVER reports a press separates the first
/// two from the third without needing to catch the moment.
/// ADC failures, which were being thrown away.
///
/// `read_single_sample_sync` returns Err if the command is refused -- BUSY,
/// say -- and `if let Ok(v)` then leaves the axis reading exactly as if the
/// stick were centred. A converter that has stopped answering and a stick
/// nobody is touching produce the same numbers, which is why the stick going
/// dead looked like nothing at all.
static mut ADC_ERRORS: [u32; 2] = [0; 2];
static mut ADC_LAST_ERR: [u16; 2] = [0; 2];
static mut ADC_OK: [u32; 2] = [0; 2];

static mut BTN_PRESSES: [u32; 2] = [0; 2];
static mut BTN_ERRORS: u32 = 0;
static mut BTN_LAST: [bool; 2] = [false; 2];

fn read_button(i: u32) -> bool {
    let now = match Buttons::read(i) {
        Ok(state) => state == libtock::buttons::ButtonState::Pressed,
        Err(_) => {
            // SAFETY: single-threaded, only writer.
            unsafe { BTN_ERRORS += 1 }
            return false;
        }
    };
    // SAFETY: single-threaded, only writer.
    unsafe {
        let k = i as usize;
        if now && !BTN_LAST[k] {
            BTN_PRESSES[k] += 1;
        }
        BTN_LAST[k] = now;
    }
    now
}

/// How many times to poll for a conversion before giving up on it.
///
/// Each poll is one non-blocking yield, so this is roughly a millisecond --
/// far longer than an RP2350 conversion, and short enough that giving up
/// costs one stale axis reading rather than a frame.
const ADC_POLL_LIMIT: u32 = 2_000;

/// Read one axis WITHOUT a wait that cannot end.
///
/// `Adc::read_single_sample_sync` spins on `yield_wait()` until the callback
/// arrives, with no bound and no way out. Putting that in a game loop means a
/// single conversion whose upcall never comes stops Doom forever -- no error,
/// no output, nothing on the console. That is exactly what wedged it on the
/// bench: the process stayed Yielded, syscalls ticked over at about twelve a
/// second, and no frame was ever drawn again.
///
/// A frame loop must not contain a wait that cannot end. This polls a bounded
/// number of times and gives up; `share::scope` unsubscribes on the way out,
/// so a late upcall is dropped rather than delivered into a dead listener, and
/// the caller carries on with the previous reading.
fn read_axis_bounded(channel: u32) -> Result<u16, libtock::platform::ErrorCode> {
    let sample: Cell<Option<u16>> = Cell::new(None);
    let listener = ADCListener(|v| sample.set(Some(v)));
    share::scope(|subscribe| {
        Adc::register_listener(&listener, subscribe)?;
        Adc::read_single_sample(channel)?;
        for _ in 0..ADC_POLL_LIMIT {
            if sample.get().is_some() {
                break;
            }
            TockSyscalls::yield_no_wait();
        }
        sample.get().ok_or(libtock::platform::ErrorCode::Busy)
    })
}

fn note_adc_error(i: usize, e: libtock::platform::ErrorCode) {
    // SAFETY: single-threaded, only writer.
    unsafe {
        ADC_ERRORS[i] += 1;
        ADC_LAST_ERR[i] = e as u16;
    }
}

fn note_axis(i: usize, v: u16) {
    // SAFETY: single-threaded, and the input path is the only writer.
    unsafe {
        AXIS_RAW[i] = v;
        if v < AXIS_MIN[i] {
            AXIS_MIN[i] = v;
        }
        if v > AXIS_MAX[i] {
            AXIS_MAX[i] = v;
        }
    }
}
static mut FRAMES: u32 = 0;

fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

#[no_mangle]
pub extern "C" fn DG_Init() {}

#[no_mangle]
pub extern "C" fn DG_SetWindowTitle(_title: *const u8) {}

#[no_mangle]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    Alarm::get_milliseconds().unwrap_or(0) as u32
}

#[no_mangle]
pub extern "C" fn DG_SleepMs(ms: u32) {
    let _ = Alarm::sleep_for(Milliseconds(ms));
}

/// Read the stick and the buttons into the six key states Doom is told about.
///
/// Two buttons cannot play Doom -- there is nothing to move or turn with -- so
/// the kit's joystick is the other half. It has two analogue axes and no click
/// button, which is exactly four directions, and the buttons are fire and use.
fn sample_input() -> [bool; 6] {
    let mut want = [false; 6];

    let (mut fwd, mut back) = (false, false);
    match read_axis_bounded(AXIS_MOVE) {
        Ok(v) => {
            fwd = v > AXIS_HIGH;
            back = v < AXIS_LOW;
            note_axis(0, v);
            // SAFETY: single-threaded, only writer.
            unsafe { ADC_OK[0] += 1 }
        }
        Err(e) => note_adc_error(0, e),
    }
    let (mut left, mut right) = (false, false);
    match read_axis_bounded(AXIS_TURN) {
        Ok(v) => {
            right = v > AXIS_HIGH;
            left = v < AXIS_LOW;
            note_axis(1, v);
            unsafe { ADC_OK[1] += 1 }
        }
        Err(e) => note_adc_error(1, e),
    }
    if INVERT_MOVE {
        core::mem::swap(&mut fwd, &mut back);
    }
    if INVERT_TURN {
        core::mem::swap(&mut left, &mut right);
    }

    want[0] = fwd;
    want[1] = back;
    want[2] = left;
    want[3] = right;
    want[4] = read_button(1);
    want[5] = read_button(0);
    want
}

/// One event per call, which is the interface doomgeneric expects: `I_GetEvent`
/// calls until this returns 0.
///
/// The inputs are sampled once per burst, not once per call: a burst is at
/// most seven calls and re-reading the converter on each would be fourteen
/// syscalls where two will do.
#[no_mangle]
pub extern "C" fn DG_GetKey(pressed: *mut i32, key: *mut u8) -> i32 {
    // SAFETY: single-threaded, and this is the only writer of these.
    unsafe {
        if !IN_BURST {
            WANT = sample_input();
            IN_BURST = true;
        }
        for i in 0..KEYS.len() {
            if WANT[i] != TOLD[i] {
                TOLD[i] = WANT[i];
                // SAFETY: doomgeneric passes two valid out-pointers.
                *pressed = if WANT[i] { 1 } else { 0 };
                *key = KEYS[i];
                return 1;
            }
        }
        IN_BURST = false;
    }
    0
}

#[no_mangle]
pub extern "C" fn DG_DrawFrame() {
    // SAFETY: `colors` and `palette_changed` are C globals this app is the
    // only Rust reader of, and Doom is not running while this callback is.
    unsafe {
        if palette_changed != 0 {
            let pal = &mut *addr_of_mut!(PALETTE);
            for i in 0..256 {
                let c = colors[i];
                pal[i] = rgb565((c >> 16) as u8, (c >> 8) as u8, c as u8);
            }
            palette_changed = 0;
        }
    }

    // SAFETY: DG_ScreenBuffer is DOOM_W * DOOM_H bytes, set by I_InitGraphics
    // before any frame is drawn.
    let fb = unsafe { core::slice::from_raw_parts(DG_ScreenBuffer, DOOM_W * DOOM_H) };
    let pal = unsafe { &*addr_of_mut!(PALETTE) };
    let stage = unsafe { &mut *addr_of_mut!(STAGE) };

    let mut y = 0usize;
    while y < DOOM_H {
        let rows = core::cmp::min(BAND, DOOM_H - y);
        let pixels = rows * DOOM_W;
        for (i, &idx) in fb[y * DOOM_W..y * DOOM_W + pixels].iter().enumerate() {
            let c = pal[idx as usize];
            // High byte first: the order hil::screen documents and the ST7796
            // takes.
            stage[i * 2] = (c >> 8) as u8;
            stage[i * 2 + 1] = c as u8;
        }
        let ok = Screen::set_write_frame(ORIGIN_X, ORIGIN_Y + y as u32, DOOM_W as u32, rows as u32)
            .and_then(|()| Screen::write(&stage[..pixels * 2]));
        if ok.is_err() {
            return; // a dropped frame is better than a wedged game
        }
        y += rows;
    }

    unsafe {
        FRAMES += 1;
    }
}

// --------------------------------------------------------------------- setup

/// The panel refuses everything with BUSY until its init sequence finishes.
fn wait_ready<W: Write>(console: &mut W) -> bool {
    let start = Alarm::get_milliseconds().unwrap_or(0);
    loop {
        match Screen::set_write_frame(0, 0, PANEL_W, PANEL_H) {
            Ok(()) => {
                let waited = Alarm::get_milliseconds().unwrap_or(0).saturating_sub(start);
                let _ = writeln!(console, "doom: panel ready after {waited} ms");
                return true;
            }
            Err(_) => {
                let waited = Alarm::get_milliseconds().unwrap_or(0).saturating_sub(start);
                if waited > READY_BUDGET_MS as u64 {
                    let _ = writeln!(console, "doom: panel never became ready");
                    return false;
                }
                let _ = Alarm::sleep_for(Milliseconds(READY_POLL_MS));
            }
        }
    }
}

// Doom may rewrite myargv, so these are mutable and static.
static mut ARG0: [u8; 5] = *b"doom\0";
static mut ARG_IWAD: [u8; 6] = *b"-iwad\0";
static mut ARG_WAD: [u8; 9] = *b"doom.wad\0";
static mut ARG_KB: [u8; 4] = *b"-kb\0";
static mut ARG_KB_N: [u8; 8] = *b"236\0\0\0\0\0";
/* Straight into the map. Without this Doom starts at the title screen and
 * wants TITLEPIC, the demo lumps and the rest of the attract loop -- none of
 * which a WAD trimmed to one map carries, and none of which this build is
 * for. It also makes the app match the configuration every measurement here
 * was taken in, which was -warp 1 1. */
static mut ARG_WARP: [u8; 6] = *b"-warp\0";
static mut ARG_EP: [u8; 2] = *b"1\0";
static mut ARG_MAP: [u8; 2] = *b"1\0";
static mut ARGV: [*mut u8; 8] = [core::ptr::null_mut(); 8];

fn main() {
    let mut console = Console::writer();
    let _ = writeln!(console, "\ndoom: starting");

    if Screen::exists().is_err() {
        let _ = writeln!(console, "doom: no screen driver in this kernel");
        return;
    }
    if !wait_ready(&mut console) {
        return;
    }

    // SAFETY: single-threaded setup, before any C code runs.
    unsafe {
        tock_heap_base = addr_of_mut!(HEAP) as *mut u8;
        tock_heap_size = HEAP_BYTES;
        tock_wad_base = WAD.as_ptr();
        tock_wad_length = WAD.len();
        tock_wad_name = WAD_NAME.as_ptr();
    }

    let wad_addr = WAD.as_ptr() as usize;
    let _ = writeln!(
        console,
        "doom: {} byte WAD at {:#x}{}",
        WAD.len(),
        wad_addr,
        if (0x1000_0000..0x1040_0000).contains(&wad_addr) {
            " (flash, read in place)"
        } else {
            " (NOT IN FLASH -- it was copied to RAM)"
        }
    );
    // The stack size comes from STACK_MEMORY, not from a literal: it was a
    // literal that said 32 KiB while the build reserved 8.
    let stack_total = unsafe { (*addr_of_mut!(STACK_MEMORY)).len() };
    let _ = writeln!(
        console,
        "doom: heap {HEAP_BYTES} bytes, zone {ZONE_KIB} KiB, stack {stack_total} bytes, \
         staging {STAGE_BYTES} bytes"
    );
    match Buttons::count() {
        Ok(n) => {
            let _ = writeln!(console, "doom: {n} buttons");
        }
        Err(e) => {
            let _ = writeln!(console, "doom: no button driver ({e:?})");
        }
    }
    match Adc::count() {
        Ok(n) => {
            let _ = writeln!(console, "doom: {n} analogue channels for the stick");
        }
        Err(e) => {
            let _ = writeln!(console, "doom: NO ADC DRIVER ({e:?}) -- no stick, no movement");
        }
    }

    // SAFETY: these statics outlive Doom, which runs until the process ends.
    unsafe {
        ARGV[0] = addr_of_mut!(ARG0) as *mut u8;
        ARGV[1] = addr_of_mut!(ARG_IWAD) as *mut u8;
        ARGV[2] = addr_of_mut!(ARG_WAD) as *mut u8;
        ARGV[3] = addr_of_mut!(ARG_KB) as *mut u8;
        ARGV[4] = addr_of_mut!(ARG_KB_N) as *mut u8;
        ARGV[5] = addr_of_mut!(ARG_WARP) as *mut u8;
        ARGV[6] = addr_of_mut!(ARG_EP) as *mut u8;
        ARGV[7] = addr_of_mut!(ARG_MAP) as *mut u8;

        paint_stack();
        let _ = writeln!(console, "doom: handing over to doomgeneric_Create\n");
        doomgeneric_Create(8, addr_of_mut!(ARGV) as *mut *mut u8);

        let _ = writeln!(console, "\ndoom: init done, entering the frame loop");
        let start = Alarm::get_milliseconds().unwrap_or(0);
        let mut reported = start;
        loop {
            doomgeneric_Tick();
            let now = Alarm::get_milliseconds().unwrap_or(0);
            if now.saturating_sub(reported) >= 5_000 {
                let secs = now.saturating_sub(start) as u32 / 1000;
                let frames = core::ptr::read_volatile(addr_of_mut!(FRAMES));
                let _ = writeln!(console, "doom: {frames} frames in {secs} s");
                report_budget(&mut console);
                reported = now;
            }
        }
    }
}
