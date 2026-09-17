use super::Cli;
use std::fs::{metadata, remove_file};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;

fn get_platform_architecture(platform: &str) -> Option<&'static str> {
    match platform {
        "raspberry_pi_pico" | "pico_explorer_base" | "nano_rp2040_connect" => Some("cortex-m0"),
        "apollo3"
        | "clue_nrf52840"
        | "hail"
        | "imix"
        | "microbit_v2"
        | "msp432"
        | "nano33ble"
        | "nrf52"
        | "nrf52840"
        | "nucleo_f429zi"
        | "nucleo_f446re"
        | "stm32f3discovery"
        | "stm32f412gdiscovery" => Some("cortex-m4"),
        "imxrt1050" | "teensy40" => Some("cortex-m7"),
        "opentitan" | "esp32_c3_devkitm_1" => Some("riscv32imc"),
        "hifive1" | "qemu_rv32_virt" => Some("riscv32imac"),
        "psc3m5_evk"
        | "raspberry_pi_pico_2"
        | "raspberry_pi_pico_2_w"
        | "raspberry_pi_pico_2_slot2"
        | "raspberry_pi_pico_2_w_slot1"
        | "raspberry_pi_pico_2_w_slot2"
        | "raspberry_pi_pico_2_w_probe"
        | "raspberry_pi_pico_2_w_hog" => Some("cortex-m33"),
        _ => None,
    }
}

// How the total TBF size must be padded, when elf2tab's own per-architecture
// default is wrong for the target.
//
// elf2tab pads every ARM TBF to a power of two, because an ARMv7-M MPU region
// must be a power of two in size and naturally aligned. ARMv8-M regions are
// 32-byte granular instead -- Tock spells the same 32 as
// CORTEXM_MIN_REGION_SIZE in arch/cortex-m33/src/mpu_v8m.rs, the MPU driver
// these boards use -- so a Cortex-M33 app would otherwise pay for a constraint
// its MPU does not have. On a Pico 2 W that caps an app at 2 MiB out of a
// 3520K region.
fn get_trailing_padding(architecture: &str) -> Option<&'static str> {
    match architecture {
        "cortex-m33" => Some("32"),

        _ => None,
    }
}

// Converts the ELF file specified on the command line into TBF and TAB files,
// and returns the paths to those files.
pub fn convert_elf(cli: &Cli, platform: &str) -> OutFiles {
    let package_name = cli.elf.file_stem().expect("ELF must be a file");
    let mut tab_path = cli.elf.clone();
    tab_path.set_extension("tab");
    if cli.verbose {
        println!("Package name: {package_name:?}");
        println!("TAB path: {}", tab_path.display());
    }
    let stack_size = read_stack_size(cli);
    let elf = cli.elf.as_os_str();
    let mut tbf_path = cli.elf.clone();
    tbf_path.set_extension("tbf");
    let architecture = get_platform_architecture(platform).unwrap_or_else(|| {
        panic!(
            "Unknown architecture for platform {platform:?}. \
             Add it to get_platform_architecture in runner/src/elf2tab.rs."
        )
    });
    if cli.verbose {
        println!("ELF file: {elf:?}");
        println!("TBF path: {}", tbf_path.display());
    }

    // If elf2tab returns a successful status but does not write to the TBF
    // file, then we run the risk of using an outdated TBF file, creating a
    // hard-to-debug situation. Therefore, we delete the TBF file, forcing
    // elf2tab to create it, and later verify that it exists.
    if let Err(io_error) = remove_file(&tbf_path) {
        // Ignore file-no-found errors, panic on any other error.
        if io_error.kind() != ErrorKind::NotFound {
            panic!("Unable to remove the TBF file. Error: {io_error}");
        }
    }

    let mut command = Command::new("elf2tab");
    #[rustfmt::skip]
    command.args([
        // TODO: libtock-rs' crates are designed for Tock 2.1's Allow interface,
        // so we should increment this as soon as the Tock kernel will accept a
        // 2.1 app.
        "--kernel-major".as_ref(), "2".as_ref(),
        "--kernel-minor".as_ref(), "0".as_ref(),
        "-n".as_ref(), package_name,
        "-o".as_ref(), tab_path.as_os_str(),
        "--stack".as_ref(), stack_size.as_ref(),
        format!("{},{}", elf.to_str().unwrap(), architecture).as_ref(),
    ]);
    if let Some(multiple) = get_trailing_padding(architecture) {
        command.args(["--trailing-padding", multiple]);
    }
    if cli.verbose {
        command.arg("-v");
        println!("elf2tab command: {command:?}");
        println!("Spawning elf2tab");
    }
    let mut child = command.spawn().expect("failed to spawn elf2tab");
    let status = child.wait().expect("failed to wait for elf2tab");
    if cli.verbose {
        println!("elf2tab finished. {status}");
    }
    assert!(status.success(), "elf2tab returned an error. {status}");

    // Verify that elf2tab created the TBF file, and that it is a file.
    match metadata(&tbf_path) {
        Err(io_error) => {
            if io_error.kind() == ErrorKind::NotFound {
                panic!("elf2tab did not create {}", tbf_path.display());
            }
            panic!(
                "Unable to query metadata for {}: {}",
                tbf_path.display(),
                io_error
            );
        }
        Ok(metadata) => {
            assert!(metadata.is_file(), "{} is not a file", tbf_path.display());
        }
    }

    OutFiles { tab_path, tbf_path }
}

// Paths to the files output by elf2tab.
pub struct OutFiles {
    pub tab_path: PathBuf,
    pub tbf_path: PathBuf,
}

// Reads the stack size, and returns it as a String for use on elf2tab's command
// line.
fn read_stack_size(cli: &Cli) -> String {
    let file = elf::File::open_path(&cli.elf).expect("Unable to open ELF");
    for section in file.sections {
        // This section name comes from runtime/libtock_layout.ld, and it
        // matches the size (and location) of the process binary's stack.
        if section.shdr.name == ".stack" {
            let stack_size = section.shdr.size.to_string();
            if cli.verbose {
                println!("Found .stack section, size: {stack_size}");
            }
            return stack_size;
        }
    }

    panic!("Unable to find the .stack section in {}", cli.elf.display());
}

/// Keeps the three hand-maintained platform lists from drifting apart.
///
/// Adding a platform takes three separate edits in three files: a `PLATFORMS`
/// row in `build_scripts/src/lib.rs`, a `platform_build` call in the
/// `Makefile`, and an arm in `get_platform_architecture` above. Nothing
/// resolves one list against another, so a platform missing from any of them
/// is invisible to every gate that compiles code -- the lists are data, and a
/// build only ever exercises the one platform it was asked for.
///
/// Found by enumeration rather than by a probe: reconciling the three lists
/// turned up `teensy40`, which the architecture map knows and the other two do
/// not. That direction is deliberate here and is asserted as such below.
#[cfg(test)]
mod platform_lists {
    use super::get_platform_architecture;
    use std::collections::BTreeSet;

    fn read(relative: &str) -> String {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../").to_string() + relative;
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
    }

    /// Names in the `PLATFORMS` table, which is a fixed-shape tuple per line.
    fn table() -> BTreeSet<String> {
        read("build_scripts/src/lib.rs")
            .lines()
            .filter_map(|line| {
                let rest = line.trim_start().strip_prefix("(\"")?;
                Some(rest.split('"').next()?.to_string())
            })
            .collect()
    }

    /// Names passed to the Makefile's `platform_build` macro.
    fn makefile() -> BTreeSet<String> {
        read("Makefile")
            .lines()
            .filter_map(|line| {
                let rest = line.split("platform_build,").nth(1)?;
                Some(rest.split(',').next()?.to_string())
            })
            .collect()
    }

    /// The parsers are the weak point of this test -- a silently empty set
    /// would make every assertion below pass. Both counts are pinned low
    /// rather than exactly, so adding a platform does not fail this.
    #[test]
    fn parsers_find_something() {
        assert!(table().len() > 20, "PLATFORMS parser found {:?}", table());
        assert!(
            makefile().len() > 20,
            "platform_build parser found {:?}",
            makefile()
        );
    }

    /// Every documented platform must be buildable from the Makefile, and
    /// every Makefile target must have a layout to build against. A target
    /// without a row panics in the build script rather than failing usefully.
    #[test]
    fn table_and_makefile_agree() {
        assert_eq!(
            table(),
            makefile(),
            "PLATFORMS and the Makefile's platform_build calls disagree"
        );
    }

    /// Every platform with a layout must have an architecture, or `elf2tab`
    /// cannot package what the build produces. This calls the real function
    /// rather than parsing its match arms.
    #[test]
    fn every_platform_has_an_architecture() {
        let missing: Vec<_> = table()
            .into_iter()
            .filter(|p| get_platform_architecture(p).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "PLATFORMS rows with no arm in get_platform_architecture: {missing:?}"
        );
    }

    /// The reverse direction is allowed, and this pins why. The architecture
    /// map may name a platform that has no layout here: `LIBTOCK_PLATFORM` for
    /// it then panics in the build script, loudly and at build time, so it is
    /// dead weight rather than a hazard. Asserted rather than left implicit so
    /// that the asymmetry is a decision and not an oversight.
    #[test]
    fn architecture_map_may_be_a_superset() {
        assert!(
            get_platform_architecture("teensy40").is_some(),
            "teensy40 lost its architecture arm; if that was deliberate, delete this test"
        );
        assert!(
            !table().contains("teensy40"),
            "teensy40 gained a PLATFORMS row -- good, but this test now asserts nothing"
        );
    }
}
