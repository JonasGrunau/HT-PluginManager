//! Cargo build script: gives the Windows exe its icon and version info.
//! (The app's "build of main" module is src/build.rs; this file is Cargo's.)

fn main() {
    println!("cargo:rerun-if-changed=assets/AppIcon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/AppIcon.ico")
            .set("ProductName", "HT Plugin Manager")
            .set("FileDescription", "HT Plugin Manager")
            .set("CompanyName", "Jonas Grunau")
            .set("LegalCopyright", "Copyright (C) 2026 Jonas Grunau")
            // The name the installer gives it (packaging/windows/ht-plugin-manager.iss).
            .set("OriginalFilename", "HT Plugin Manager.exe")
            .set("InternalName", "ht-plugin-manager");
        res.compile().expect("compile the Windows resources");
    }
}
