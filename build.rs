fn main() {
    // Commit written in the log's first line, so a log can be matched to its source.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=HORAMA_GIT_COMMIT={}", commit);
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    // Declaring files above replaces Cargo's default "rerun on any change": list the rest.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(target_os = "windows")]
    {
        if std::path::Path::new("assets/icon.ico").exists() {
            let mut res = winres::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.set("ProductName", "Horama");
            res.set(
                "FileDescription",
                "Horama - Color-Managed RAW and Image Viewer",
            );
            let _ = res.compile();
        }
    }
}
