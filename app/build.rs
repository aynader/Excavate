fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("the UI doesn't compile");
    // Icons (app, tray for dark and light taskbars), the manifest (per-monitor DPI, segment heap, UTF-8) and version.
    println!("cargo:rerun-if-changed=excavate.rc");
    println!("cargo:rerun-if-changed=excavate.manifest");
    println!("cargo:rerun-if-changed=../brand/generated");
    embed_resource::compile("excavate.rc", version()).manifest_required().expect("resources");
}

/// The version in Cargo.toml, for the resource script: Windows shows it in each program's Properties → Details.
fn version() -> [String; 2] {
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    [format!("VERSION_NUMBERS={},{},{},0", part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH")),
     format!("VERSION_TEXT=\"{}\"", part("CARGO_PKG_VERSION"))]
}
