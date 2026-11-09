use std::{env, fs, path::PathBuf};

fn main() {
    // Put the linker script where the linker searches.
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy("rustboot.x", out.join("rustboot.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg-bins=-Trustboot.x");
    println!("cargo:rustc-link-arg-bins=--gc-sections");
    println!("cargo:rerun-if-changed=rustboot.x");
    println!("cargo:rerun-if-changed=keys/pubkey.bin");
    println!("cargo:rerun-if-changed=keys/lms_pubkey.bin");
}
