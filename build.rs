//! Media Foundation (video, screen share, audio playback) is loaded on first
//! use instead of at start, so Windows "N" editions without the Media Feature
//! Pack can still open Ferrochat; `voice::decoder::startup` checks it's there.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for dll in ["mfplat.dll", "mfreadwrite.dll"] {
            println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-lib=delayimp");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
