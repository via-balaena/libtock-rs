//! The kit's panel through the screen capsule, drawing what `kit_display_bars`
//! draws over raw SPI.
//!
//! ```text
//! make raspberry_pi_pico_2_w EXAMPLE=kit_screen_bars
//! ```
//!
//! Needs a kernel built with the board feature that wires the panel to
//! userspace. Without it `Screen::exists()` fails and this app says so and
//! stops.
//!
//! # Why this exists
//!
//! `kit_display_bars` drives the panel from userspace over SPI, setting
//! `MADCTL`, `COLMOD` and `INVON` itself. The screen capsule drives the same
//! glass through `capsules_extra::st77xx`, which does its own init. Every fact
//! in the raw-SPI app was measured on this panel and then written into that
//! capsule variant, so the two paths were derived from one set of measurements
//! and have never been compared. This app is the comparison: same pattern, same
//! glass, the other path.
//!
//! Neither session could run it alone -- the capsule is one repo and the app is
//! another -- which is the whole reason it is worth building.
//!
//! # What to look for, in the order it fails
//!
//! The console says all of this before anything is drawn, so the run can be
//! read afterwards without anyone having watched it.
//!
//! * **Resolution.** The capsule is expected to report 480 wide by 320 high:
//!   it sets `MV`, so the controller is in landscape before the first pixel is
//!   addressed. A report of 320x480 would mean the capsule and this panel's
//!   mounting disagree, and the bars would come out along the short axis.
//!   The layout below follows whatever is reported, so a disagreement shows up
//!   as bars running the wrong way rather than as an error.
//!
//! * **Pixel format.** Expected 2. That is `RGB_565` in both the enum this
//!   repo's pinned kernel carries and the one on the bench, which is worth
//!   saying because the two enums are otherwise *not* the same -- see the note
//!   on the ordinals below.
//!
//! * **The eight bars, drawn with `fill`.** White, yellow, cyan, green,
//!   magenta, red, blue, black, left to right. `Screen::fill` takes a `u16` and
//!   chooses the byte order itself -- high byte first
//!   (`apis/display/screen/src/lib.rs:236-237`).
//!
//! * **The bottom strip, drawn with `write`.** Blue on the left half, white on
//!   the right. `Screen::write` passes this app's bytes through untouched, so
//!   the order here is the app's own choice and is written out literally.
//!
//! Splitting the pattern across the two calls is what makes a byte-order fault
//! legible instead of merely visible:
//!
//! | bars | strip | what it means |
//! |---|---|---|
//! | right | right | both paths agree with the capsule |
//! | wrong | right | the wrapper's `fill` order disagrees with the capsule |
//! | right | wrong | this app's literal bytes are the wrong way round |
//! | wrong | wrong | the capsule's RGB565 order is not high-byte-first |
//!
//! "Wrong" for a byte swap is specific and not a matter of opinion: with the
//! two bytes exchanged, white and black are unmoved, yellow and blue trade,
//! cyan and red trade, and green and magenta trade.
//!
//! Two other failures have their own signatures, both measured on this panel
//! during the raw-SPI bring-up:
//!
//! * **Every colour its exact complement** -- black glass, white bars -- is the
//!   missing `INVON`. This panel is IPS and wants inversion; `kit_display_paint`
//!   filled it four times to establish that.
//! * **Red and blue exchanged while white, black, green and magenta stay put**
//!   is the `BGR` bit in `MADCTL`. That set is exactly what survives swapping
//!   the red and blue channels, which is how it was identified the first time.
//!
//! # The strip is also a prediction about `write`, with its own control
//!
//! The two halves of the strip are drawn differently on purpose.
//!
//! **Left half, blue: one `write` call per row.** In the kernel this repo pins
//! (`tock/capsules/extra/src/screen.rs`, `b3234a172`), every syscall-level
//! write resets the position before it starts: `ScreenCommand::Write` sets
//! `app.write_position = 0` at `:260` and then calls
//! `self.screen.write(data, false)` at `:269` -- `continue_write` false, which
//! `st77xx` takes as "go back to the frame origin". The capsule only passes
//! `true` from inside `write_complete` (`:436`), continuing *within* one
//! syscall, never across two.
//!
//! **So the prediction is that all forty calls paint row zero, and the left
//! half comes out as a one-pixel blue line with black underneath it.**
//!
//! **Right half, white: one `write` call for the whole rectangle.** This is the
//! control, and it is the path a frame blit would actually use -- one large
//! buffer, chunked by the capsule through its own smaller one, `continue_write`
//! true for every chunk after the first. It should paint completely.
//!
//! Read the two together:
//!
//! | left | right | what it means |
//! |---|---|---|
//! | one blue line | solid white | writes restart at the frame origin; predicted |
//! | solid blue | solid white | writes continue across calls; the bench kernel changed this |
//! | anything | not solid | the write path is broken, and the left half says nothing |
//!
//! The control is the point. Without it a blank left half reads as "write is
//! broken" exactly as readily as "write restarts", and those have opposite
//! consequences for anyone building a frame blit on top of this.
//!
//! The referent above is eighteen months older than the kernel on the bench --
//! this repo pins `b3234a172` from 2024-04-05 -- but the kernel side has since
//! confirmed the same three lines in their own tree, at `:235`, `:271` and
//! `:440`, unchanged across that whole span. **So a solid blue left half is now
//! a surprise rather than merely a finding**, and the prediction is about the
//! kernel actually on the bench rather than about an old pin.
//!
//! The two trees are known to have diverged
//! already: that kernel's `ScreenPixelFormat` is
//! `Mono, RGB_233, RGB_565, RGB_888, ARGB_8888` (`kernel/src/hil/screen.rs:98-114`)
//! where the bench kernel has `Mono, RGB_332, RGB_565, RGB_888, BGRA_8888,
//! RGB_4BIT, Mono_8BitPage`. Index 2 is `RGB_565` in both, which is the only
//! one this app depends on, but indices 1 and 4 were *renamed* -- and a rename
//! from `ARGB_8888` to `BGRA_8888` is a channel-order change, not a spelling.
//!
//! The strip is drawn on a black ground so that undrawn rows are unambiguously
//! undrawn rather than whatever the panel happened to be holding.
//!
//! # BUSY during init, and why the retry is here rather than in the wrapper
//!
//! The first run of this app, 2026-09-14, got the geometry line right and then
//! **eleven BUSY answers in a row, the first call included.** The cause is not
//! this app and not the wrapper: `st77xx` answers BUSY while its `Status` is
//! anything but `Idle`, and getting to `Idle` takes **1240 ms from `init()`** --
//! a 370 ms hardware-reset ladder before the init sequence, then 870 ms of
//! sequence delays. Eleven syscalls fit inside that window with room to spare.
//!
//! **870, not the 625 those constants appear to sum to.** `delay: 255` is a
//! sentinel rather than a duration: `do_next_op` maps it to 500
//! (`tock/capsules/extra/src/st77xx.rs:545-547`). So `SLEEP_OUT` is really 500 --
//! 40% of the whole gap in one command -- and `SW_RESET`'s `delay: 150` carries
//! a `// 255?` comment, meaning a change to 255 there would silently become 500
//! as well. Read the mapping, never the constant.
//!
//! **A kernel client waits for `screen_is_ready()`. An app cannot.** The
//! capsule implements readiness and uses it to run queued commands, but nothing
//! surfaces "the screen has finished initialising" through the syscall
//! interface. That is a real gap and it is the kernel side's; retrying on BUSY
//! is the workaround, not the fix.
//!
//! It lives in this app rather than in `apis/display/screen` deliberately.
//! Putting a retry loop in the wrapper would change what every screen call in
//! the crate does about time, which is a PR with a description attached. Here
//! it changes one example.
//!
//! **How long the wait actually is has never been measured from userspace**, so
//! the app reports it: the total time spent absorbing BUSY is printed at the
//! end. That number is the size of the gap, stated in the units an app
//! experiences it in.
//!
//! The first run also found a kernel defect that would have made this retry
//! useless -- `enqueue_command` left `pending_command` set when the screen
//! refused a command, so the first BUSY wedged the app's access for the life of
//! the process and every later call answered BUSY from the queue rather than
//! from the driver. Fixed kernel-side in `c45dd2150`. A retry against the
//! unfixed kernel would have spun forever and looked like a dead panel.
//!
//! # What `Ok` means here, which is less than it looks
//!
//! **`Ok(())` from any of these calls does not mean the operation succeeded.**
//! Every completing screen operation in this crate is written as subscribe,
//! command, then spin on `yield_wait` until an upcall lands, and the upcall's
//! status argument is then thrown away: `if let Some((_,)) = called.get() {
//! return Ok(()) }`, ten times over in `apis/display/screen/src/lib.rs`. The
//! kernel does report the asynchronous outcome there --
//! `run_next_command(into_statuscode(r), 0, 0)` at
//! `tock/capsules/extra/src/screen.rs:426, :439, :450` -- and this crate
//! discards it.
//!
//! So a synchronous rejection is caught, by `to_result()?` on the command, and
//! an asynchronous failure is reported as success. For this app that means
//! **"every call returned Ok" is a statement about the calls, not about the
//! glass**, and the console says so rather than implying otherwise. The glass
//! is the only result.
//!
//! It also means a specific misreading is available and worth naming: if the
//! single large control write fails inside the capsule, this app prints one
//! write call of N bytes and no error, and the right half stays black. That is
//! the "write path is broken" row of the table above, and the console will not
//! say which part broke.

#![no_main]
#![no_std]

use core::fmt::Write;

use libtock::alarm::{Alarm, Milliseconds};
use libtock::console::Console;
use libtock::display::Screen;
use libtock::platform::ErrorCode;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x8000}

/// RGB565, as a `u16` for `fill`, which picks the byte order.
const WHITE: u16 = 0xffff;
const YELLOW: u16 = 0xffe0;
const CYAN: u16 = 0x07ff;
const GREEN: u16 = 0x07e0;
const MAGENTA: u16 = 0xf81f;
const RED: u16 = 0xf800;
const BLUE: u16 = 0x001f;
const BLACK: u16 = 0x0000;

/// The same two colours as literal bytes, high byte first, for `write` -- the
/// point being that this app chose the order rather than the wrapper.
const BLUE_BYTES: [u8; 2] = [0x00, 0x1f];
const WHITE_BYTES: [u8; 2] = [0xff, 0xff];

/// Broadcast order, left to right.
const BARS: [(u16, &str); 8] = [
    (WHITE, "white"),
    (YELLOW, "yellow"),
    (CYAN, "cyan"),
    (GREEN, "green"),
    (MAGENTA, "magenta"),
    (RED, "red"),
    (BLUE, "blue"),
    (BLACK, "black"),
];

/// Holds the whole right half of the strip in one buffer: 240 by 40 pixels at
/// two bytes each, on a 480-wide panel. If the reported geometry needs more
/// than this, the control is skipped rather than silently truncated.
const CONTROL_MAX: usize = 240 * 40 * 2;

/// How long to sleep between retries of a call the panel refused with BUSY,
/// and how long to keep doing that before giving up. The init sequence this is
/// absorbing is upwards of 600 ms; the budget is generous because the cost of
/// being wrong is a run that reports a dead panel.
const READY_POLL_MS: u32 = 25;
const READY_BUDGET_MS: u32 = 5_000;

fn main() {
    let mut console = Console::writer();

    if Screen::exists().is_err() {
        let _ = writeln!(
            console,
            "kit_screen_bars: no screen driver at 0x90001.\r\n\
             The kernel needs the board feature that wires the panel to \
             userspace.\r\n\
             A test-only build that keeps the capsule's client back lands \
             here too.\r"
        );
        return;
    }

    let (width, height) = match Screen::get_resolution() {
        Ok(pair) => pair,
        Err(e) => {
            let _ = writeln!(console, "kit_screen_bars: get_resolution: {e:?}\r");
            return;
        }
    };
    let format = Screen::get_pixel_format();

    let _ = writeln!(
        console,
        "kit_screen_bars: capsule reports {width} x {height}, pixel format \
         {format:?}\r"
    );
    let _ = writeln!(
        console,
        "  expected 480 x 320 and format 2. A report of 320 x 480 means the \
         capsule and\r\n\
         \x20 this panel's mounting disagree about which axis is which; the \
         bars below\r\n\
         \x20 follow the report, so they run along the short edge if so. \
         Format 2 is\r\n\
         \x20 RGB_565 in both the pinned and the bench kernel; the enums \
         differ at 1 and 4.\r"
    );

    if width < 16 || height < 16 {
        let _ = writeln!(
            console,
            "kit_screen_bars: {width} x {height} is too small to lay out \
             eight bars\r"
        );
        return;
    }

    let bar_w = width / 8;
    let bars_h = height * 7 / 8;
    let strip_y = bars_h;
    let strip_h = height - bars_h;
    let left_w = width / 2;
    let right_w = width - left_w;
    let control_bytes = (right_w as usize) * (strip_h as usize) * 2;

    let _ = writeln!(
        console,
        "  drawing: eight bars {bar_w} wide and {bars_h} tall with fill, left \
         to right\r\n\
         \x20 white yellow cyan green magenta red blue black; then a strip \
         {strip_h} tall\r\n\
         \x20 on a black ground, blue across the left {left_w} and white \
         across the right {right_w}.\r"
    );
    let _ = writeln!(
        console,
        "  bars wrong and strip right means the wrapper's fill byte order is \
         wrong;\r\n\
         \x20 bars right and strip wrong means this app's literal bytes are; \
         both wrong\r\n\
         \x20 means the capsule does not want high byte first. A swap leaves \
         white and\r\n\
         \x20 black alone and trades yellow/blue, cyan/red, green/magenta.\r"
    );
    let _ = writeln!(
        console,
        "  PREDICTED: the left half paints one blue row and no more, because \
         every\r\n\
         \x20 syscall write resets the position -- screen.rs:260 sets \
         write_position = 0\r\n\
         \x20 and :269 passes continue_write false. The right half goes in \
         one call and\r\n\
         \x20 should be solid. A solid blue left half means the bench kernel \
         differs from\r\n\
         \x20 the one pinned here, which is a finding. A right half that is \
         not solid\r\n\
         \x20 means the write path is broken and the left half says nothing \
         at all.\r"
    );

    let mut failures = 0u32;
    let mut waited_total = 0u32;
    let mut waited_first = 0u32;

    for (index, (colour, name)) in BARS.iter().enumerate() {
        let x = bar_w * index as u32;
        let (result, waited) = fill_rect(x, 0, bar_w, bars_h, *colour);
        if index == 0 {
            waited_first = waited;
        }
        waited_total += waited;
        if let Err(e) = result {
            let _ = writeln!(console, "kit_screen_bars: fill {name}: {e:?}\r");
            failures += 1;
        }
    }

    if waited_first > 0 {
        let _ = writeln!(
            console,
            "kit_screen_bars: the first call waited {waited_first} ms for the \
             panel to leave\r\n\
             \x20 its init sequence -- that is the size of the readiness gap, \
             measured from\r\n\
             \x20 userspace, where no syscall reports it.\r"
        );
    }

    // Black ground, so an undrawn row of the strip is unambiguously undrawn.
    let (grounded, waited) = fill_rect(0, strip_y, width, strip_h, BLACK);
    waited_total += waited;
    if let Err(e) = grounded {
        let _ = writeln!(console, "kit_screen_bars: fill strip ground: {e:?}\r");
        failures += 1;
    }

    let (left, waited) = write_rect_by_rows(0, strip_y, left_w, strip_h, BLUE_BYTES);
    waited_total += waited;
    match left {
        Ok(calls) => {
            let _ = writeln!(
                console,
                "kit_screen_bars: left half took {calls} write calls of \
                 {} bytes each\r",
                left_w * 2
            );
        }
        Err(e) => {
            let _ = writeln!(console, "kit_screen_bars: left half: {e:?}\r");
            failures += 1;
        }
    }

    if control_bytes > CONTROL_MAX {
        let _ = writeln!(
            console,
            "kit_screen_bars: right half needs {control_bytes} bytes and the \
             buffer holds\r\n\
             \x20 {CONTROL_MAX}; the control is skipped, so the left half \
             answers nothing.\r"
        );
        failures += 1;
    } else {
        let mut buffer = [0u8; CONTROL_MAX];
        for pair in buffer[..control_bytes].chunks_exact_mut(2) {
            pair.copy_from_slice(&WHITE_BYTES);
        }
        let (right, waited) =
            write_rect_once(left_w, strip_y, right_w, strip_h, &buffer[..control_bytes]);
        waited_total += waited;
        match right {
            Ok(()) => {
                let _ = writeln!(
                    console,
                    "kit_screen_bars: right half took 1 write call of \
                     {control_bytes} bytes\r"
                );
            }
            Err(e) => {
                let _ = writeln!(console, "kit_screen_bars: right half: {e:?}\r");
                failures += 1;
            }
        }
    }

    let _ = writeln!(
        console,
        "kit_screen_bars: {waited_total} ms total spent retrying calls the \
         panel refused\r\n\
         \x20 with BUSY. Anything beyond the first call's wait means BUSY \
         arrived after\r\n\
         \x20 the panel was up, which would be a different finding.\r"
    );

    if failures == 0 {
        let _ = writeln!(
            console,
            "kit_screen_bars: every call returned Ok -- which says the calls \
             were accepted\r\n\
             \x20 and an upcall arrived, and nothing more. This crate discards \
             the upcall's\r\n\
             \x20 status argument in all ten completing screen operations, so \
             an operation\r\n\
             \x20 that failed inside the capsule reports Ok here. The glass is \
             the result.\r"
        );
    } else {
        let _ = writeln!(
            console,
            "kit_screen_bars: {failures} calls failed; the glass is a partial \
             drawing\r"
        );
    }

    loop {
        let _ = Alarm::sleep_for(Milliseconds(1000));
    }
}

/// Runs one drawing call, absorbing the BUSY the panel returns for as long as
/// its init sequence lasts. Returns the outcome and how long it waited, so the
/// size of that window is reported rather than merely tolerated.
///
/// Only BUSY is retried. Every other error is the answer.
fn retry_busy(mut call: impl FnMut() -> Result<(), ErrorCode>) -> (Result<(), ErrorCode>, u32) {
    let mut waited = 0;
    loop {
        match call() {
            Err(ErrorCode::Busy) if waited < READY_BUDGET_MS => {
                let _ = Alarm::sleep_for(Milliseconds(READY_POLL_MS));
                waited += READY_POLL_MS;
            }
            result => return (result, waited),
        }
    }
}

/// One solid rectangle, colour chosen as a `u16` so the wrapper picks the byte
/// order. The frame and the fill retry together: a frame the panel refused has
/// to be set again before the fill it belongs to.
fn fill_rect(x: u32, y: u32, w: u32, h: u32, colour: u16) -> (Result<(), ErrorCode>, u32) {
    retry_busy(|| {
        Screen::set_write_frame(x, y, w, h)?;
        let mut pixel = [0u8; 2];
        Screen::fill(&mut pixel, colour)
    })
}

/// One rectangle built from the caller's own bytes, a row per `write` call.
/// Returns the call count, so a capsule that restarts at the frame origin can
/// be told from one that continues -- by comparing the count against how many
/// rows actually painted.
fn write_rect_by_rows(
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    pixel: [u8; 2],
) -> (Result<u32, ErrorCode>, u32) {
    // One row. 480 is the widest panel this app expects, and a half-width row
    // of it is 240 pixels.
    let mut row = [0u8; 240 * 2];
    let row_bytes = (w as usize) * 2;
    if row_bytes > row.len() {
        return (Err(ErrorCode::Size), 0);
    }
    for pair in row[..row_bytes].chunks_exact_mut(2) {
        pair.copy_from_slice(&pixel);
    }

    let (framed, mut waited) = retry_busy(|| Screen::set_write_frame(x, y, w, h));
    if let Err(e) = framed {
        return (Err(e), waited);
    }

    let mut calls = 0u32;
    for _ in 0..h {
        let (written, spent) = retry_busy(|| Screen::write(&row[..row_bytes]));
        waited += spent;
        if let Err(e) = written {
            return (Err(e), waited);
        }
        calls += 1;
    }
    (Ok(calls), waited)
}

/// One rectangle in a single `write` call -- the path a frame blit uses, where
/// the capsule chunks the caller's buffer through its own and continues within
/// the one syscall.
fn write_rect_once(x: u32, y: u32, w: u32, h: u32, pixels: &[u8]) -> (Result<(), ErrorCode>, u32) {
    retry_busy(|| {
        Screen::set_write_frame(x, y, w, h)?;
        Screen::write(pixels)
    })
}
