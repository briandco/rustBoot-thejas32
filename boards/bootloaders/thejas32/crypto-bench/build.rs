use std::{env, path::PathBuf};

fn main() {
    let dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rustc-link-search={}", dir.display());
    println!("cargo:rustc-link-arg-bins=-Tmemory.x");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=--gc-sections");
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-env-changed=BENCH_VERSION");
}
