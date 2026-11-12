fn main() {
    println!("cargo:rerun-if-changed=indexer.rc");
    println!("cargo:rerun-if-changed=helper.rc");
    println!("cargo:rerun-if-changed=indexer.manifest");
    // Each executable its own name in Task Manager: "Excavate Reader", "Excavate Helper".
    embed_resource::compile_for("indexer.rc", ["excavate-indexer"], version()).manifest_optional().unwrap();
    embed_resource::compile_for("helper.rc", ["excavate-helper"], version()).manifest_optional().unwrap();

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

/// The version in Cargo.toml, for the resource script: Windows shows it in each program's Properties → Details.
fn version() -> [String; 2] {
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    [format!("VERSION_NUMBERS={},{},{},0", part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH")),
     format!("VERSION_TEXT=\"{}\"", part("CARGO_PKG_VERSION"))]
}
