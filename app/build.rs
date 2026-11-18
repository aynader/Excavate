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
    delay_load_media();
}

/// Media Foundation's libraries load when first used rather than when the program starts: Windows' N editions have
/// none until the Media Feature Pack is added, and a program that imports them outright doesn't start there at all.
/// Each use asks `winsys::media::available` first.
fn delay_load_media() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for dll in ["mfplat.dll", "mfreadwrite.dll"] {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
}

/// The version in Cargo.toml, for the resource script: Windows shows it in each program's Properties → Details, and
/// the manifest has it (`MANIFEST`: excavate.manifest with its `$VERSION` filled in, written beside the build's
/// output), so a new version is one edit, in Cargo.toml.
fn version() -> [String; 3] {
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let (major, minor, patch) = (part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH"));
    let template = std::fs::read_to_string("excavate.manifest").expect("excavate.manifest");
    assert!(template.contains("$VERSION"), "excavate.manifest has no $VERSION for the version to go in");
    let manifest = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("excavate.manifest");
    std::fs::write(&manifest, template.replace("$VERSION", &format!("{major}.{minor}.{patch}.0"))).expect("the manifest");
    [format!("VERSION_NUMBERS={major},{minor},{patch},0"),
     format!("VERSION_TEXT=\"{}\"", part("CARGO_PKG_VERSION")),
     format!("MANIFEST=\"{}\"", manifest.display().to_string().replace('\\', "/"))]
}
