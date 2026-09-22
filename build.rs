fn main() {
    println!("cargo:rustc-check-cfg=cfg(native_updater_bridge)");
    println!("cargo:rerun-if-env-changed=CODEX_NATIVE_UPDATER_PATH");
    if let Some(path) = std::env::var_os("CODEX_NATIVE_UPDATER_PATH") {
        let path = std::path::PathBuf::from(path)
            .canonicalize()
            .expect("native updater path");
        println!("cargo:rerun-if-changed={}", path.display());
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
            .join("windows-updater.node");
        std::fs::copy(path, output).expect("embedding native updater");
        println!("cargo:rustc-cfg=native_updater_bridge");
    } else if std::env::var("PROFILE").as_deref() == Ok("release") {
        panic!("Release builds require the updater sidecar. Run ./build.ps1 -Release");
    }
    slint_build::compile("ui/app.slint").expect("Slint build failed");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        // Icon is embedded later when we have one; placeholder only for now.
        if std::path::Path::new("assets/installer.ico").exists() {
            res.set_icon("assets/installer.ico");
        }
        res.compile().expect("Windows resource embed failed");
    }
}
