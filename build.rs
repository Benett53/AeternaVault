//! Embeds the application icon and version information into the executables.
//!
//! On MSVC this uses `rc.exe` from the Windows SDK (available on GitHub's
//! `windows-latest` runners). On other toolchains a resource compiler may be
//! missing; the build then continues without an embedded icon instead of failing.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/aeternavault.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/web/style.css");

    // A short fingerprint of the web style sheet: part of its address, so
    // browsers load a changed one instead of a kept copy.
    let css = std::fs::read("src/web/style.css").unwrap_or_default();
    let hash = css.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    });
    println!("cargo:rustc-env=AV_STYLE_HASH={:08x}", hash as u32);

    #[cfg(windows)]
    embed_windows_resources();
}

#[cfg(windows)]
fn embed_windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon/aeternavault.ico")
        .set("ProductName", "AeternaVault")
        .set("FileDescription", "AeternaVault")
        .set("OriginalFilename", "aeternavault.exe")
        .set("InternalName", "AeternaVault")
        .set("LegalCopyright", "MIT OR Apache-2.0")
        .set("ProductVersion", &version)
        .set("FileVersion", &version);

    if let Err(err) = res.compile() {
        println!("cargo:warning=Windows resources (icon, version info) were not embedded: {err}");
    }
}
