#![no_main]
#![no_std]
use core::fmt::Write;
use libtock::alarm::Alarm;
use libtock::alarm::Milliseconds;
use libtock::console::Console;
use libtock::display::Screen;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {11*1024}

fn main() {
    // Output a message to the console
    let _ = writeln!(Console::writer(), "available resolutions\n");

    // Query the number of supported Screen resolution modes
    let resolutions = match Screen::get_resolution_modes_count() {
        Ok(val) => val,
        Err(e) => {
            let _ = writeln!(Console::writer(), "{e:?}\n");
            0
        }
    };

    // Exit if no resolutions found
    if resolutions == 0 {
        assert_eq!(0, 1);
    }

    let _ = writeln!(Console::writer(), "{resolutions}\n");

    // Iterate over each resolution mode and print the width and height
    for index in 0..resolutions {
        let (width, height) = match Screen::get_resolution_width_height(index as usize) {
            Ok((width, height)) => (width, height),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
                (0, 0)
            }
        };

        let width = (width, height).0;
        let height = (width, height).1;

        // Abort if invalid resolution
        if width == 0 && height == 0 {
            assert_eq!(0, 1);
        }

        let _ = writeln!(Console::writer(), " {width} x {height} \n");
    }

    // Print supported color depths
    let _ = writeln!(Console::writer(), "available colors depths\n");

    let pixel_format = match Screen::get_pixel_format() {
        Ok(val) => val,
        Err(e) => {
            let _ = writeln!(Console::writer(), "{e:?}\n");
            0
        }
    };

    if pixel_format == 0 {
        assert_eq!(0, 1);
    }

    // List each supported color format
    for index in 0..pixel_format {
        let format = match Screen::pixel_format(index as usize) {
            Ok(val) => val,
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
                0
            }
        };
        let _ = writeln!(Console::writer(), "  {format} bpp\n");
    }

    const BUFFER_SIZE: usize = 10 * 1024;

    // Initialize the Screen screen buffer
    let mut buffer = [0u8; BUFFER_SIZE];

    // Set Screen brightness to 100%
    match Screen::set_brightness(100) {
        Ok(()) => (),
        Err(e) => {
            let _ = writeln!(Console::writer(), "{e:?}\n");
        }
    };

    // Get current screen resolution
    let (width, height) = match Screen::get_resolution() {
        Ok((width, height)) => (width, height),
        Err(e) => {
            let _ = writeln!(Console::writer(), "{e:?}\n");
            (0, 0)
        }
    };

    // Unwrap width and height
    let width = (width, height).0;
    let height = (width, height).1;

    if width == 0 && height == 0 {
        assert_eq!(0, 1);
    };

    // Set full-screen write frame and clear screen
    match Screen::set_write_frame(0, 0, width, height) {
        Ok(()) => (),
        Err(e) => {
            let _ = writeln!(Console::writer(), "{e:?}\n");
        }
    };
    match Screen::fill(&mut buffer, 0x0) {
        Ok(()) => (),
        Err(e) => {
            let _ = writeln!(Console::writer(), "{e:?}\n");
        }
    };

    // The squares below sit at y=24 with height 24, and both figures are
    // multiples of 8 on purpose.
    //
    // `ssd1306` and `sh1106` address their panels in 8-row pages and cannot
    // express a frame that straddles one. They used to accept a misaligned
    // frame and quietly reshape it -- a 20-row request painted 16 rows and
    // returned `Ok(())` -- and since tock `45c16e8c5` they answer `INVAL`
    // instead. This example previously asked for y=20, height=30, which is
    // exactly that shape.
    //
    // **The constraint belongs to those drivers, not to the syscall API**, so
    // `Screen::set_write_frame` still takes any rectangle and the wrapper does
    // not enforce alignment. A multiple of 8 is simply valid everywhere, which
    // is what a generic example wants: this one has no idea which panel it is
    // running on. `x` and `width` are unconstrained -- paging is rows.
    let mut invert = false;
    for i in 0.. {
        // Every 4 iterations, toggle Screen inversion
        if i % 4 == 3 {
            invert = !invert;
            if invert {
                let _ = Screen::set_invert_on();
            } else {
                let _ = Screen::set_invert_off();
            }
        }

        // Set Screen rotation (0 to 3)
        match Screen::set_rotation(i % 4) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };

        // Draw a red square at (10, 20)
        match Screen::set_write_frame(10, 24, 30, 24) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };
        match Screen::fill(&mut buffer, 0xF800) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };

        // Draw a black square at (88, 20)
        match Screen::set_write_frame(88, 24, 30, 24) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };
        match Screen::fill(&mut buffer, 0x0) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };

        // Wait 1 second
        Alarm::sleep_for(Milliseconds(1000)).unwrap();

        // Clear the red square
        match Screen::set_write_frame(10, 24, 30, 24) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };
        match Screen::fill(&mut buffer, 0x0) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };

        // Draw a green square at (88, 20)
        match Screen::set_write_frame(88, 24, 30, 24) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };
        match Screen::fill(&mut buffer, 0x07F0) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };

        // Wait 1 second
        Alarm::sleep_for(Milliseconds(1000)).unwrap();

        // Clear screen
        match Screen::set_write_frame(0, 0, width, height) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };
        match Screen::fill(&mut buffer, 0x0) {
            Ok(()) => (),
            Err(e) => {
                let _ = writeln!(Console::writer(), "{e:?}\n");
            }
        };
    }
}
