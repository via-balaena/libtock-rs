use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    libtock_build_scripts::auto_layout();

    // Only the `doom` example needs this, and it needs a checkout of
    // doomgeneric that this repository does not vendor.
    if std::env::var_os("CARGO_FEATURE_DOOM").is_some() {
        build_doom();
    }
}

/// Compile doomgeneric plus its libc shim for the target and hand the objects
/// to the linker.
///
/// The objects go in as link arguments rather than as a static archive on
/// purpose: an archive would need an `ar` that understands ELF for this target
/// (the host's does not on macOS), and Doom needs nearly every object anyway,
/// so there is nothing for archive member selection to save.
///
/// The compile itself lives in the doomgeneric tree's own `tools/build_arm.sh`
/// so the flags have one home. `-mfloat-abi=soft` in particular has to match
/// what Rust builds, and a second copy of that decision here would be a second
/// place for it to drift.
fn build_doom() {
    let src = std::env::var("DOOM_SRC").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME must be set to find doomgeneric");
        format!("{home}/forge/doomgeneric")
    });
    let src = PathBuf::from(&src);
    let builder = src.join("tools/build_arm.sh");
    assert!(
        builder.is_file(),
        "no doomgeneric at {}: set DOOM_SRC to a checkout with tools/build_arm.sh",
        src.display()
    );

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("doomobj");
    let status = Command::new(&builder)
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", builder.display()));
    assert!(status.success(), "{} failed", builder.display());

    let mut objects: Vec<PathBuf> = std::fs::read_dir(&out)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", out.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "o").unwrap_or(false))
        .collect();
    // Deterministic link order, so two builds of the same sources produce the
    // same binary.
    objects.sort();
    assert!(
        objects.len() > 50,
        "{} produced only {} objects, which cannot be all of Doom",
        out.display(),
        objects.len()
    );

    for obj in &objects {
        println!("cargo:rustc-link-arg={}", obj.display());
    }

    // Rebuild when the C changes.
    for dir in [src.join("doomgeneric"), src.join("tock")] {
        watch(&dir);
    }
    println!("cargo:rerun-if-changed={}", builder.display());
    println!("cargo:rerun-if-env-changed=DOOM_SRC");
}

fn watch(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            watch(&path);
        } else if path
            .extension()
            .map(|e| e == "c" || e == "h")
            .unwrap_or(false)
        {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
