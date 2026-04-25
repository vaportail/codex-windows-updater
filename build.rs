fn main() {
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
