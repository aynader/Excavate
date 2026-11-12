//! voidtools' SDKs (MIT, sdk/), compiled in as static code: SDK3 talks to Everything 1.5 over its named pipe, the 1.4
//! SDK over window messages. Nothing to ship beside the executable and no process per search.

fn main() {
    println!("cargo:rerun-if-changed=sdk");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    cc::Build::new()
        .file("sdk/sdk1/src/Everything.c")
        .define("EVERYTHINGUSERAPI", Some(""))
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .warnings(false)
        .compile("everything_sdk1");
    cc::Build::new()
        .file("sdk/sdk3/src/Everything3.c")
        .define("EVERYTHING3_LIB", None)
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .warnings(false)
        .compile("everything_sdk3");
    for library in ["user32", "ole32", "oleaut32", "shell32", "propsys", "advapi32"] {
        println!("cargo:rustc-link-lib={library}");
    }
}
