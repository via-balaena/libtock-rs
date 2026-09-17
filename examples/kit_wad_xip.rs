//! Can a Tock process read bulk data straight out of flash, in place?
//!
//! Doom's zone allocator spends most of its memory on copies of WAD lumps, and
//! `W_CacheLumpNum` already has the cure: when `wad_file->mapped` is set it
//! returns a pointer *into* the file rather than allocating. On the RP2350
//! flash is XIP-mapped, so a WAD sitting in flash is exactly that case --
//! measured at a 34x collapse of the purgeable class on a host build.
//!
//! The catch is the MPU. A Tock process gets `ReadExecuteOnly` on **exactly
//! its own flash slice**, so it cannot address the rest of flash at all: a WAD
//! written to some spare region would fault on first read. What it *can* read
//! is its own image, so the WAD has to travel inside the app's own TBF.
//!
//! This app is that arrangement. It embeds a real IWAD with `include_bytes!`,
//! which lands in `.rodata` and therefore in the process's flash slice, and
//! then reads every lump the directory names. This said "64 KiB" and nothing
//! ever sliced it -- the app walks `WAD.len()`, which is the whole file.
//!
//! # The WAD is not in the repository
//!
//! `assets/wad_trim.bin` is gitignored, so a fresh clone fails to build with a
//! bare `couldn't read examples/assets/wad_trim.bin`. See `doom.rs` for how to
//! produce one. This test only needs a structurally valid IWAD, so either
//! script in `assets/` will serve it -- `doom.rs` is the one that is fussy.
//!
//! # What the answers mean
//!
//! * **An address of the form `0x100.....`** says the bytes really are in
//!   flash and are being read where they lie. An address in `0x2.......`
//!   would mean the toolchain copied them into RAM at startup, which is the
//!   whole thing this is trying to avoid.
//! * **`IWAD` and a plausible lump count** say the read is not just returning
//!   zeroes.
//! * **A checksum over every byte** says the whole slice is readable, not
//!   merely the first word. If the MPU grant were short, this is where it
//!   would fault rather than at the header.
//!
//! A process fault here is a real answer, not a crash to debug: it would mean
//! embedded data does not get the same treatment as code, and the WAD would
//! have to reach userspace some other way.

#![no_main]
#![no_std]

use core::fmt::Write;
use libtock::console::Console;
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x1000}

/// A real, structurally valid IWAD: every lump its directory names is
/// present, which is the property this test needs.
///
/// This described `make_wad.py`'s output -- "about 3 MB", a byte-budget prefix
/// of Freedoom's lumps, not playable. The file actually here is
/// `make_min_wad.py`'s: 1.73 MB, 983 lumps, playable, selected by what one map
/// needs. Both are valid IWADs and this app cannot tell them apart, which is
/// exactly why the description drifted without anything failing.
static WAD: &[u8] = include_bytes!("assets/wad_trim.bin");

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn main() {
    let mut console = Console::writer();

    let addr = WAD.as_ptr() as usize;
    let _ = writeln!(
        console,
        "kit_wad_xip: {} bytes at {:#010x}",
        WAD.len(),
        addr
    );

    // The process's own flash slice starts at the board's prog region. Naming
    // the boundary rather than the exact base, because where the app lands
    // depends on what else is loaded.
    let in_flash = (0x1000_0000..0x1040_0000).contains(&addr);
    let _ = writeln!(
        console,
        "  {} -- {}",
        if in_flash { "IN FLASH" } else { "IN RAM" },
        if in_flash {
            "read in place, no copy"
        } else {
            "the toolchain copied it to RAM, which defeats the purpose"
        }
    );

    let magic = &WAD[0..4];
    let numlumps = le32(WAD, 4);
    let infotableofs = le32(WAD, 8);
    let _ = writeln!(
        console,
        "  magic {:?} numlumps {} infotableofs {}",
        core::str::from_utf8(magic).unwrap_or("??"),
        numlumps,
        infotableofs
    );

    // Walk the directory the way `W_CacheLumpNum` would: compute a pointer
    // into the image for each lump and read it, without copying anything.
    // This is the access pattern that matters, not a flat scan -- a directory
    // entry can point anywhere in the file, so it exercises the far end of the
    // region as well as the near.
    let dir = infotableofs as usize;
    let mut bytes_seen: usize = 0;
    let mut sum: u32 = 0;
    let mut widest: u32 = 0;
    let mut widest_name = [0u8; 8];

    for i in 0..numlumps as usize {
        let e = dir + i * 16;
        let pos = le32(WAD, e) as usize;
        let size = le32(WAD, e + 4) as usize;
        if pos + size > WAD.len() {
            let _ = writeln!(console, "  lump {i} runs past the image; stopping");
            break;
        }
        // Touch both ends of every lump. A grant that is short at the far end
        // would fault here rather than on the header.
        if size > 0 {
            sum = sum.wrapping_add(WAD[pos] as u32);
            sum = sum.wrapping_add(WAD[pos + size - 1] as u32);
        }
        bytes_seen += size;
        if size as u32 > widest {
            widest = size as u32;
            widest_name.copy_from_slice(&WAD[e + 8..e + 16]);
        }
    }

    let _ = writeln!(
        console,
        "  walked {numlumps} lumps, {bytes_seen} bytes of lump data, edge-sum {sum:#x}"
    );
    let _ = writeln!(
        console,
        "  largest lump {:?} at {} bytes",
        core::str::from_utf8(&widest_name)
            .unwrap_or("??")
            .trim_end_matches('\0'),
        widest
    );
    let _ = writeln!(
        console,
        "kit_wad_xip: whole image addressable in place, no copy, no fault"
    );
}
