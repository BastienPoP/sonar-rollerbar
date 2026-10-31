//! Embeds the application icon and manifest in the executable.

use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/sonar-rollerbar.rc");
    println!("cargo:rerun-if-changed=assets/sonar-rollerbar.ico");
    println!("cargo:rerun-if-changed=assets/sonar-rollerbar.manifest");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let object = Path::new(&out_dir).join("sonar-rollerbar-resources.o");
    let status = Command::new("x86_64-w64-mingw32-windres")
        .args(["--input", "assets/sonar-rollerbar.rc"])
        .args(["--output-format", "coff", "--output"])
        .arg(&object)
        .status()
        .expect("x86_64-w64-mingw32-windres comes with gcc-mingw-w64-x86-64");
    assert!(status.success(), "windres failed");

    println!("cargo:rustc-link-arg-bins={}", object.display());
}
