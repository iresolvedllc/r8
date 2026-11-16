use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let zig_dir = manifest.join("../../zig");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let lib_dir = out.join("lib");
    std::fs::create_dir_all(&lib_dir).expect("create lib dir");

    println!("cargo:rerun-if-changed={}", zig_dir.join("src").display());

    let status = Command::new("zig")
        .arg("build-lib")
        .arg("src/root.zig")
        .arg("-static")
        .arg("-fPIC")
        .arg("-O")
        .arg("Debug")
        .arg(format!(
            "-femit-bin={}",
            lib_dir.join("libr8_machine.a").display()
        ))
        .current_dir(&zig_dir)
        .status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("zig build-lib failed with {status}"),
        Err(error) => panic!("could not run zig (install Zig 0.17): {error}"),
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=r8_machine");
}
