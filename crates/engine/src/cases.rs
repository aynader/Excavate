//! A folder of awkward files, to compare what each index makes of them; and a burst of changes, to compare how each
//! keeps up. Everything here is made inside the folder given (and its sibling `outside` for moves in and out).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn long(path: &Path) -> PathBuf {
    // Joined as OS strings: `display()` would turn an unpaired surrogate into U+FFFD.
    let mut long = std::ffi::OsString::from(r"\\?\");
    long.push(path.as_os_str());
    PathBuf::from(long)
}

fn touch(path: &Path) {
    if let Err(e) = fs::write(long(path), b"x") {
        eprintln!("  couldn't make {}: {e}", path.display());
    }
}

/// Names the plain Win32 path rules would refuse or change, made through \\?\.
pub const NAMES: &[&str] = &[
    "plain.txt", "Report Final.docx", "no_extension", ".dotfile", "dots...txt", "trailing dot.", "trailing space ",
    " leading space.txt", "con.txt", "aux", "nul.log", "[brackets].txt", "a&b;c,d'e!f^g%h~i$j.txt", "#hash", "100% done.txt",
    "café.txt",            // é as one character (NFC)
    "cafe\u{301}-nfd.txt", // e + combining accent (NFD)
    "naive.txt", "naïve.txt", "Straße.txt", "STRASSE.txt", "İstanbul.txt", "ΣΊΣΥΦΟΣ.txt", "ﬁle-ligature.txt",
    "日本語のファイル.txt", "🦀 crab.txt", "Ünïcödé Mixed.TXT", "tab\u{9}name.txt",
    // Torture: Turkish dotless ı, capital ẞ, more ligatures, right-to-left text and a right-to-left override that
    // disguises an extension, joined and flag emoji, a variation selector, stacked combining marks, NFC and NFD twins
    // in one folder (NTFS allows both), half- and full-width forms, and a name of 255 characters.
    "ıi dotless.txt", "ẞig sharp.txt", "ﬀ ligature.txt", "שלום עולם.txt", "مرحبا بالعالم.txt", "photo\u{202E}gpj.exe",
    "👩\u{200D}💻 coder.txt", "🇪🇬 flag.txt", "❤\u{FE0F} heart.txt", "Z\u{337}\u{322}a\u{338}l\u{335}g\u{336}o.txt",
    "twin é.txt", "twin e\u{301}.txt", "ｶﾀｶﾅ half.txt", "カタカナ full.txt", "ＡＢＣ wide.txt",
];

/// Names that aren't valid UTF-16 (an unpaired surrogate each), which only the wide API can make.
pub fn make_invalid(root: &Path) {
    use std::os::windows::ffi::OsStringExt;
    for (n, unit) in [0xD800u16, 0xDBFF, 0xDC00, 0xDFFF].into_iter().enumerate() {
        let mut name: Vec<u16> = format!("lone-{n}-").encode_utf16().collect();
        name.push(unit);
        name.extend(".txt".encode_utf16());
        let path = root.join(std::ffi::OsString::from_wide(&name));
        if let Err(e) = fs::write(long(&path), b"x") {
            eprintln!("  couldn't make a name with an unpaired surrogate: {e}");
        }
    }
}

pub fn make(root: &Path) {
    let _ = fs::create_dir_all(long(root));
    for name in NAMES.iter().filter(|n| !n.is_empty()) {
        touch(&root.join(name));
    }
    make_invalid(root);
    touch(&root.join(format!("{}.txt", "n".repeat(251))));
    // Long paths: past 260 characters.
    let mut deep = root.join("long");
    for n in 0..12 {
        deep = deep.join(format!("a-rather-long-folder-name-{n:02}"));
    }
    let _ = fs::create_dir_all(long(&deep));
    touch(&deep.join("at-the-bottom-of-a-long-path.txt"));
    // A very deep tree.
    let mut tree = root.join("deep");
    for n in 0..120 {
        tree = tree.join(format!("{n}"));
    }
    let _ = fs::create_dir_all(long(&tree));
    touch(&tree.join("deepest.txt"));
    // Many in one folder.
    let bulk = root.join("bulk");
    let _ = fs::create_dir_all(&bulk);
    for n in 0..5000 {
        touch(&bulk.join(format!("f{n:05}.txt")));
    }
    let _ = fs::create_dir_all(root.join("empty folder"));
    let _ = fs::write(root.join("zero-bytes.bin"), b"");
    // Hard links: one file, three names, two folders.
    touch(&root.join("hard-original.txt"));
    let _ = fs::create_dir_all(root.join("links"));
    let _ = fs::hard_link(root.join("hard-original.txt"), root.join("hard-link.txt"));
    let _ = fs::hard_link(root.join("hard-original.txt"), root.join("links").join("hard-link-elsewhere.txt"));
    // A junction and a symbolic link to a folder with a file in it.
    let target = root.join("target-folder");
    let _ = fs::create_dir_all(&target);
    touch(&target.join("inside-target.txt"));
    let junction = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"]).arg(root.join("junction")).arg(&target).output();
    if !junction.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("  no junction");
    }
    if std::os::windows::fs::symlink_dir(&target, root.join("symlink-folder")).is_err() {
        eprintln!("  no symbolic link (needs Developer Mode or admin)");
    }
    // An alternate data stream.
    touch(&root.join("with-stream.txt"));
    let _ = fs::write(format!(r"{}\with-stream.txt:hidden-stream", root.display()), b"y");
    // Hidden and system.
    touch(&root.join("hidden.txt"));
    touch(&root.join("system.txt"));
    let _ = std::process::Command::new("attrib").arg("+h").arg(root.join("hidden.txt")).status();
    let _ = std::process::Command::new("attrib").arg("+s").arg(root.join("system.txt")).status();
    // Sizes past 1 TB (sparse, so no disk is used) and dates outside what 32 bits of seconds since 1970 hold.
    sparse(&root.join("sparse-2tb.bin"), 2 << 40);
    date(&root.join("from-1900.txt"), UNIX_EPOCH - Duration::from_secs(70 * 365 * 86_400));
    date(&root.join("from-2200.txt"), UNIX_EPOCH + Duration::from_secs(230 * 365 * 86_400));
    // To rename and move later.
    let _ = fs::create_dir_all(root.join("moving").join("sub"));
    touch(&root.join("moving").join("sub").join("moved-with-folder.txt"));
    touch(&root.join("delete-me.txt"));
    touch(&root.join("recreate-me.txt"));
    touch(&root.join("rename-chain-0.txt"));
    touch(&root.join("case only.txt"));
}

fn date(path: &Path, when: SystemTime) {
    touch(path);
    if let Ok(file) = fs::OpenOptions::new().write(true).open(long(path)) {
        let _ = file.set_modified(when);
    }
}

fn sparse(path: &Path, size: u64) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let Ok(file) = fs::File::create(long(path)) else { return };
    let mut got = 0u32;
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle() as _, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0,
                        &mut got, std::ptr::null_mut())
    };
    if ok == 0 || file.set_len(size).is_err() {
        eprintln!("  no sparse file");
    }
}

/// Removes what `make` and `mutate` made under `folder` (its `cases` and `outside`, and `folder` if that empties it).
/// Through \?\ for the long and oddly named paths; junctions and links are removed, not followed.
pub fn remove(folder: &Path) {
    for part in ["cases", "outside"] {
        let path = folder.join(part);
        if path.exists() {
            if let Err(e) = fs::remove_dir_all(long(&path)) {
                eprintln!("  couldn't remove {}: {e}", path.display());
            }
        }
    }
    let _ = fs::remove_dir(folder);
}

/// The changes, in order: what an index must keep up with.
pub fn mutate(root: &Path, outside: &Path) {
    let r = |p: &str| long(&root.join(p));
    // Case only.
    let _ = fs::rename(r("case only.txt"), r("CASE ONLY.txt"));
    // A rename chain, as fast as it goes.
    for n in 0..30 {
        let _ = fs::rename(r(&format!("rename-chain-{n}.txt")), r(&format!("rename-chain-{}.txt", n + 1)));
    }
    let _ = fs::remove_file(r("delete-me.txt"));
    let _ = fs::remove_file(r("recreate-me.txt"));
    let _ = fs::write(r("recreate-me.txt"), b"again");
    // A folder renamed with what's inside.
    let _ = fs::rename(r("moving"), r("moved"));
    // A folder moved in from outside, and one moved out.
    let incoming = outside.join("incoming");
    let _ = fs::create_dir_all(incoming.join("nested"));
    let _ = fs::write(incoming.join("nested").join("came-in-with-folder.txt"), b"x");
    let _ = fs::rename(long(&incoming), r("incoming"));
    let _ = fs::rename(r("links"), long(&outside.join("links-moved-out")));
    // A burst.
    let _ = fs::create_dir_all(r("burst"));
    for n in 0..3000 {
        let _ = fs::write(r(&format!("burst\\b{n:04}.txt")), b"x");
    }
    // Made, renamed and deleted before anyone looks.
    let _ = fs::write(r("blink.txt"), b"x");
    let _ = fs::rename(r("blink.txt"), r("blink2.txt"));
    let _ = fs::remove_file(r("blink2.txt"));
}
