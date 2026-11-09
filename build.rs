//! Media Foundation (video, screen share, audio playback) is loaded on first
//! use instead of at start, so Windows "N" editions without the Media Feature
//! Pack can still open Ferrochat; `voice::decoder::startup` checks it's there.
//!
//! The exe also gets its icon (assets/ferrochat.ico), compiled with the
//! Windows SDK's resource compiler, `rc.exe`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        for dll in ["mfplat.dll", "mfreadwrite.dll"] {
            println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-lib=delayimp");
        embed_icon();
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/ferrochat.ico");
}

/// Without `rc.exe` on PATH (outside a VS developer prompt) the exe still
/// builds, just with the default icon.
fn embed_icon() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("ferrochat.ico");
    let script = out.join("ferrochat.rc");
    let resource = out.join("ferrochat.res");
    let written = std::fs::write(&script, format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "\\\\")));
    let compile = |rc: &std::path::Path| {
        std::process::Command::new(rc).arg("/nologo").arg("/fo").arg(&resource).arg(&script).status().is_ok_and(|s| s.success())
    };
    let compiled = written.is_ok() && (compile("rc.exe".as_ref()) || sdk_rc().is_some_and(|rc| compile(&rc)));
    if compiled {
        println!("cargo:rustc-link-arg-bins={}", resource.display());
    } else {
        println!("cargo:warning=rc.exe not found: the exe gets no icon (install the Windows 10/11 SDK to add it)");
    }
}

/// The newest Windows SDK's x64 `rc.exe`, for when it isn't on PATH.
fn sdk_rc() -> Option<std::path::PathBuf> {
    let bin = std::path::PathBuf::from(std::env::var_os("ProgramFiles(x86)")?).join("Windows Kits").join("10").join("bin");
    let mut versions: Vec<std::path::PathBuf> = std::fs::read_dir(bin).ok()?.flatten().map(|entry| entry.path().join("x64").join("rc.exe")).filter(|rc| rc.exists()).collect();
    versions.sort();
    versions.pop()
}
