//! Holds the screen driver busy, so app B's writes have something to collide
//! with. App A of the pair; see `screen_busy_probe`.
//!
//! ```text
//! make raspberry_pi_pico_2_w_slot1 EXAMPLE=screen_busy_load
//! ```
//!
//! # Why `fill` and not `write`
//!
//! The load wants the driver occupied for as long as possible per syscall.
//! `fill` sends **two bytes** and the capsule replicates them into its own
//! 12,800-byte buffer, chunking until the frame is covered — so a full-screen
//! fill is about 40 ms of wire time for 2 bytes of RAM. A `write` of the same
//! area would need a 307,200-byte buffer, which does not fit in the slot's
//! 128K grant. The point is occupancy, and `fill` buys far more of it.
//!
//! # It prints almost nothing, on purpose
//!
//! A console line blocks for the UART, and this app exists to be *in* the
//! driver, not waiting on a serial port. Anything it printed per iteration
//! would be time it was not holding the driver, which is the one thing it is
//! for. It reports at the end, and app B carries the measurement.
//!
//! # Reading it with B
//!
//! B's `max` write latency should land near one of these fills. If it does
//! not, this app was not actually loading the driver and B's clean result
//! means nothing — that check is why this one reports its fill count and
//! timing at all.

#![no_main]
#![no_std]

use core::cell::Cell;
use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};
use libtock_platform::allow_ro::AllowRo;
use libtock_platform::share;
use libtock_platform::subscribe::Subscribe;
use libtock_platform::{DefaultConfig, ErrorCode, Syscalls};
use libtock_runtime::TockSyscalls;

set_main! {main}
stack_size! {0x1000}

const SCREEN: u32 = 0x9_0001;
const SCREEN_EXISTS: u32 = 0;
const GET_RESOLUTION: u32 = 23;
const SET_WRITE_FRAME: u32 = 100;
const FILL: u32 = 300;
const SCREEN_CB: u32 = 0;
const WRITE_BUF: u32 = 0;

/// Alternated so the panel visibly churns and a stalled app A is obvious by
/// eye, without costing a syscall to decide.
const COLOURS: [u16; 2] = [0x001F, 0x0000];

/// Long enough to outlast app B's run, so B never stops contending early.
const RUN_MS: u32 = 12_000;

fn main() {
    let mut console = Console::writer();

    let hz = Alarm::get_frequency().map(|f| f.0).unwrap_or(1).max(1);
    if !TockSyscalls::command(SCREEN, SCREEN_EXISTS, 0, 0).is_success() {
        let _ = writeln!(
            console,
            "busy_load: no screen driver at 0x90001 -- build the board with \
             kit_display\r"
        );
        return;
    }
    let (w, h) = TockSyscalls::command(SCREEN, GET_RESOLUTION, 0, 0)
        .get_success_2_u32()
        .unwrap_or((480, 320));

    let t_start = Alarm::get_ticks().unwrap_or(0);
    let mut fills = 0u32;
    let mut errors = 0u32;
    let mut slowest_us = 0u32;

    loop {
        let now = Alarm::get_ticks().unwrap_or(0);
        if ((now.wrapping_sub(t_start) as u64 * 1_000) / hz as u64) as u32 >= RUN_MS {
            break;
        }
        if blocking_frame(0, 0, w, h).is_err() {
            errors += 1;
            continue;
        }
        let a = Alarm::get_ticks().unwrap_or(0);
        if blocking_fill(COLOURS[(fills as usize) & 1]).is_err() {
            errors += 1;
        } else {
            fills += 1;
        }
        let b = Alarm::get_ticks().unwrap_or(0);
        let elapsed = ((b.wrapping_sub(a) as u64 * 1_000_000) / hz as u64) as u32;
        slowest_us = slowest_us.max(elapsed);
    }

    let _ = writeln!(
        console,
        "busy_load: {fills} full-screen fills, {errors} errors, slowest \
         {slowest_us} us\r\n\
         \x20 app B's max write latency should land near that figure; if it does \
         not, B was not\r\n\
         \x20 contending with anything and its result is void.\r"
    );
}

/// `set_write_frame`, blocking. No deadline here: this app is the load, and a
/// stall in it shows up in B's numbers, which is where the measurement lives.
///
/// The capsule packs x/y into `data1` and width/height into `data2`; they are
/// not interchangeable, and swapping them asks for a zero-area frame rather
/// than failing.
fn blocking_frame(x: u32, y: u32, w: u32, h: u32) -> Result<(), ErrorCode> {
    let data1 = ((x & 0xFFFF) << 16) | (y & 0xFFFF);
    let data2 = ((w & 0xFFFF) << 16) | (h & 0xFFFF);
    let called: Cell<Option<(u32,)>> = Cell::new(None);
    share::scope(|subscribe| {
        TockSyscalls::subscribe::<_, _, DefaultConfig, SCREEN, SCREEN_CB>(subscribe, &called)?;
        TockSyscalls::command(SCREEN, SET_WRITE_FRAME, data1, data2)
            .to_result::<(), ErrorCode>()?;
        loop {
            TockSyscalls::yield_wait();
            if let Some((status,)) = called.get() {
                return if status == 0 {
                    Ok(())
                } else {
                    Err(ErrorCode::Fail)
                };
            }
        }
    })
}

/// `fill`: two bytes over the syscall boundary, the whole frame on the wire.
fn blocking_fill(colour: u16) -> Result<(), ErrorCode> {
    let pixel = colour.to_be_bytes();
    let called: Cell<Option<(u32,)>> = Cell::new(None);
    share::scope::<
        (
            AllowRo<_, SCREEN, WRITE_BUF>,
            Subscribe<_, SCREEN, SCREEN_CB>,
        ),
        _,
        _,
    >(|handle| {
        let (allow, subscribe) = handle.split();
        TockSyscalls::allow_ro::<DefaultConfig, SCREEN, WRITE_BUF>(allow, &pixel)?;
        TockSyscalls::subscribe::<_, _, DefaultConfig, SCREEN, SCREEN_CB>(subscribe, &called)?;
        TockSyscalls::command(SCREEN, FILL, 0, 0).to_result::<(), ErrorCode>()?;
        loop {
            TockSyscalls::yield_wait();
            if let Some((status,)) = called.get() {
                return if status == 0 {
                    Ok(())
                } else {
                    Err(ErrorCode::Fail)
                };
            }
        }
    })
}
