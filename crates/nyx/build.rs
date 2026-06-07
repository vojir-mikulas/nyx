//! Windows-only: embed an app icon and version metadata into `nyx.exe` so it
//! shows an icon in Explorer and carries version strings in its properties.
//!
//! Gated by `#[cfg(windows)]` (the build script's *host*), which matches the
//! host-gated build-dependencies below — the release `.exe` is produced by a
//! native Windows runner, so host == target there. A no-op on every other host.

fn main() {
    #[cfg(windows)]
    embed_windows_resources();
}

#[cfg(windows)]
fn embed_windows_resources() {
    use std::{env, fs::File, path::PathBuf};

    let png = "../../assets/nyx.png";
    println!("cargo:rerun-if-changed={png}");

    // Build a multi-resolution .ico from the 1024² source PNG.
    let src = image::open(png).expect("read assets/nyx.png");
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 32, 48, 64, 128, 256] {
        let rgba = src
            .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        let image = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
        icon_dir.add_entry(ico::IconDirEntry::encode(&image).expect("encode ico entry"));
    }
    let ico_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("nyx.ico");
    icon_dir
        .write(File::create(&ico_path).expect("create nyx.ico"))
        .expect("write nyx.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().unwrap());
    res.set("ProductName", "Nyx");
    res.set("FileDescription", "Nyx — a fast, reliable SFTP/FTP client");
    res.set("LegalCopyright", "Apache-2.0");
    // FileVersion / ProductVersion are filled from CARGO_PKG_VERSION by winresource.
    res.compile().expect("embed Windows resources");
}
