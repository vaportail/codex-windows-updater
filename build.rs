fn main() {
    println!("cargo:rerun-if-env-changed=CODEX_IDENTITY_SHIM_PATH");
    let shim_out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
        .join("codex_identity_shim.dll");
    if let Some(path) = std::env::var_os("CODEX_IDENTITY_SHIM_PATH") {
        println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new(&path).display()
        );
        std::fs::copy(path, shim_out).expect("copy identity shim for embedding");
    } else {
        // Development builds can use a separately built sibling DLL.
        std::fs::write(shim_out, []).expect("write empty shim placeholder");
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
