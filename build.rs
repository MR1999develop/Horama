fn main() {
    slint_build::compile("ui/app.slint").expect("Failed to compile Slint UI");

    #[cfg(target_os = "windows")]
    {
        if std::path::Path::new("assets/icon.ico").exists() {
            let mut res = winres::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.set("ProductName", "Horama");
            res.set(
                "FileDescription",
                "Horama - Color-Managed RAW & Image Viewer",
            );
            let _ = res.compile();
        }
    }
}
