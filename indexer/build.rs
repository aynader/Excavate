fn main() {
    println!("cargo:rerun-if-changed=indexer.rc");
    println!("cargo:rerun-if-changed=helper.rc");
    println!("cargo:rerun-if-changed=bedrock.rc");
    println!("cargo:rerun-if-changed=setup.rc");
    println!("cargo:rerun-if-changed=indexer.manifest");
    // Each executable its own name in Task Manager: "Excavate Reader", "Excavate Helper".
    embed_resource::compile_for("indexer.rc", ["excavate-indexer"], version()).manifest_optional().unwrap();
    embed_resource::compile_for("helper.rc", ["excavate-helper"], version()).manifest_optional().unwrap();
    embed_resource::compile_for("bedrock.rc", ["excavate-bedrock"], version()).manifest_optional().unwrap();
    embed_resource::compile_for("setup.rc", ["excavate-setup"], version()).manifest_optional().unwrap();
    // The setup runs alone, on a Windows that may not have Microsoft's C++ runtime (VCRUNTIME140.dll): the runtime is
    // linked into it. The other programs find it beside them.
    for arg in ["/NODEFAULTLIB:msvcrt.lib", "/NODEFAULTLIB:vcruntime.lib", "/NODEFAULTLIB:ucrt.lib",
                "/DEFAULTLIB:libcmt.lib", "/DEFAULTLIB:libvcruntime.lib", "/DEFAULTLIB:libucrt.lib"] {
        println!("cargo:rustc-link-arg-bin=excavate-setup={arg}");
    }
    delay_load_media();

    // The libraries beside the executables, where they're looked for, each fetched once into vendor/:
    // PDFium (scripts/fetch-pdfium.sh; without it, PDFs are listed as unreadable and everything else works), and
    // ONNX Runtime with DirectML (scripts/fetch-onnxruntime.sh; without them, text in images isn't read).
    for dll in ["../vendor/pdfium/bin/pdfium.dll", "../vendor/onnxruntime/onnxruntime.dll", "../vendor/onnxruntime/DirectML.dll"] {
        let dll = std::path::Path::new(dll);
        println!("cargo:rerun-if-changed={}", dll.display());
        if let (true, Ok(out)) = (dll.exists(), std::env::var("OUT_DIR")) {
            // OUT_DIR is target/<profile>/build/<crate>-<hash>/out.
            if let Some(profile) = std::path::Path::new(&out).ancestors().nth(3) {
                let target = profile.join(dll.file_name().unwrap());
                let stale = std::fs::metadata(&target).map(|m| m.len()).ok() != std::fs::metadata(dll).map(|m| m.len()).ok();
                if stale {
                    let _ = std::fs::remove_file(&target);
                    let _ = std::fs::copy(dll, target);
                }
            }
        }
    }
}

/// The version in Cargo.toml, for the resource script: Windows shows it in each program's Properties → Details, and
/// the manifest has it (`MANIFEST`: indexer.manifest with its `$VERSION` filled in, written beside the build's
/// output), so a new version is one edit, in Cargo.toml.
fn version() -> [String; 3] {
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let (major, minor, patch) = (part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH"));
    let template = std::fs::read_to_string("indexer.manifest").expect("indexer.manifest");
    assert!(template.contains("$VERSION"), "indexer.manifest has no $VERSION for the version to go in");
    let manifest = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("indexer.manifest");
    std::fs::write(&manifest, template.replace("$VERSION", &format!("{major}.{minor}.{patch}.0"))).expect("the manifest");
    [format!("VERSION_NUMBERS={major},{minor},{patch},0"),
     format!("VERSION_TEXT=\"{}\"", part("CARGO_PKG_VERSION")),
     format!("MANIFEST=\"{}\"", manifest.display().to_string().replace('\\', "/"))]
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
