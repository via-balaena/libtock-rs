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
use libtock::alarm::{Alarm, Milliseconds};
use libtock::buttons::Buttons;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}

/// Doom recurses through the BSP, so this is not a formality. Not yet measured
/// -- 32 KiB is a guess with room in it, and the first thing to shrink if the
/// zone needs more.
stack_size! {0x8000}

const DOOM_W: usize = 320;
const DOOM_H: usize = 200;
const PANEL_W: u32 = 480;
const PANEL_H: u32 = 320;
const ORIGIN_X: u32 = (PANEL_W - DOOM_W as u32) / 2;
const ORIGIN_Y: u32 = (PANEL_H - DOOM_H as u32) / 2;

/// Rows per blit. 50 was the fastest band `kit_doom_frame` measured and costs
/// 32,000 bytes of staging; it divides 200 exactly, so no band is short.
const BAND: usize = 50;
const STAGE_BYTES: usize = DOOM_W * BAND * 2;

/// What the shim's allocator hands out. Doom's zone takes nearly all of it in
/// one call, so this and `ZONE_KIB` move together.
const HEAP_BYTES: usize = 320 * 1024;
/// Passed to Doom as `-kb`. Leaves the heap a few KiB for the handful of
/// strings Doom duplicates outside the zone.
const ZONE_KIB: usize = 316;

/// The panel spends over a second in its init sequence and answers BUSY until
/// it is done. Measured at 1225 ms from userspace.
const READY_POLL_MS: u32 = 25;
const READY_BUDGET_MS: u32 = 5_000;

// Doom's own, from doomkeys.h and m_controls.c's defaults.
const KEY_FIRE: u8 = 0x80 + 0x1d; // KEY_RCTRL
const KEY_USE: u8 = b' ';

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

#[no_mangle]
pub extern "C" fn tock_exit(status: i32) -> ! {
    let mut console = Console::writer();
    let _ = writeln!(console, "\ndoom: exit({status})");
    loop {
        let _ = Alarm::sleep_for(Milliseconds(1000));
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
}

static mut PALETTE: [u16; 256] = [0; 256];
static mut STAGE: [u8; STAGE_BYTES] = [0; STAGE_BYTES];
static mut BUTTON_WAS: [bool; 2] = [false; 2];
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

/// One event per call, which is the interface doomgeneric expects: it calls
/// until this returns 0. Two buttons, reported on edges.
#[no_mangle]
pub extern "C" fn DG_GetKey(pressed: *mut i32, key: *mut u8) -> i32 {
    for b in 0..2u32 {
        let now = Buttons::is_pressed(b);
        // SAFETY: single-threaded, and this is the only writer.
        let was = unsafe { &mut *addr_of_mut!(BUTTON_WAS) };
        if now != was[b as usize] {
            was[b as usize] = now;
            // SAFETY: doomgeneric passes two valid out-pointers.
            unsafe {
                *pressed = if now { 1 } else { 0 };
                *key = if b == 0 { KEY_USE } else { KEY_FIRE };
            }
            return 1;
        }
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
static mut ARG_KB_N: [u8; 8] = *b"316\0\0\0\0\0";
static mut ARGV: [*mut u8; 5] = [core::ptr::null_mut(); 5];

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
    let _ = writeln!(
        console,
        "doom: heap {} bytes, zone {} KiB, stack 32 KiB, staging {} bytes",
        HEAP_BYTES, ZONE_KIB, STAGE_BYTES
    );
    match Buttons::count() {
        Ok(n) => {
            let _ = writeln!(console, "doom: {n} buttons");
        }
        Err(e) => {
            let _ = writeln!(console, "doom: no button driver ({e:?})");
        }
    }

    // SAFETY: these statics outlive Doom, which runs until the process ends.
    unsafe {
        ARGV[0] = addr_of_mut!(ARG0) as *mut u8;
        ARGV[1] = addr_of_mut!(ARG_IWAD) as *mut u8;
        ARGV[2] = addr_of_mut!(ARG_WAD) as *mut u8;
        ARGV[3] = addr_of_mut!(ARG_KB) as *mut u8;
        ARGV[4] = addr_of_mut!(ARG_KB_N) as *mut u8;

        let _ = writeln!(console, "doom: handing over to doomgeneric_Create\n");
        doomgeneric_Create(5, addr_of_mut!(ARGV) as *mut *mut u8);

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
                reported = now;
            }
        }
    }
}
