//! The kind filters. Each is defined once, by extension, and used two ways: as Everything search syntax for the long
//! tail, and as a test on hot-tier entries, so both tiers agree on what "Images" means.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Filter {
    #[default]
    All,
    Apps,
    Folders,
    Documents,
    Pictures,
    Audio,
    Video,
    Code,
    Archives,
}

pub const DOCUMENTS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "odt", "rtf", "txt", "md", "markdown", "xls", "xlsx", "xlsm", "ods", "csv", "ppt",
    "pptx", "odp", "epub", "pages", "numbers", "key", "tex", "msg", "eml", "one", "xps",
];
pub const PICTURES: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "avif", "jxl", "tif", "tiff", "svg", "ico", "psd",
    "raw", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf",
];
pub const AUDIO: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "wma", "aiff", "aif", "mid", "midi", "amr"];
pub const VIDEO: &[&str] = &["mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v", "mpg", "mpeg", "3gp", "flv", "ts", "mts", "m2ts"];
pub const CODE: &[&str] = &[
    "rs", "py", "ipynb", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cc", "cpp", "cxx", "hpp", "cs", "java", "kt",
    "kts", "go", "rb", "php", "swift", "m", "mm", "scala", "lua", "pl", "r", "jl", "dart", "sql", "sh", "bash", "ps1",
    "psm1", "bat", "cmd", "vue", "svelte", "html", "htm", "css", "scss", "less", "json", "yaml", "yml", "toml", "xml",
    "ini", "cfg", "gradle", "cmake", "zig", "hs", "ex", "exs", "erl", "clj", "elm", "ml", "fs", "slint", "proto",
    "graphql", "tf", "nix", "glsl", "hlsl", "wgsl", "cu", "asm", "vb", "lock", "sln", "csproj", "vcxproj",
];
pub const ARCHIVES: &[&str] = &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "iso", "img", "vhd", "vhdx", "wim", "msi", "msix", "appx"];
pub const APPS: &[&str] = &["exe", "lnk", "appref-ms", "msc", "cpl", "url"];

impl Filter {
    pub const ALL: [Filter; 9] = [Filter::All, Filter::Apps, Filter::Documents, Filter::Pictures, Filter::Code,
                                  Filter::Audio, Filter::Video, Filter::Folders, Filter::Archives];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Apps => "Apps",
            Filter::Folders => "Folders",
            Filter::Documents => "Documents",
            Filter::Pictures => "Images",
            Filter::Audio => "Audio",
            Filter::Video => "Video",
            Filter::Code => "Code",
            Filter::Archives => "Archives",
        }
    }

    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Filter::Documents => DOCUMENTS,
            Filter::Pictures => PICTURES,
            Filter::Audio => AUDIO,
            Filter::Video => VIDEO,
            Filter::Code => CODE,
            Filter::Archives => ARCHIVES,
            Filter::Apps => APPS,
            Filter::All | Filter::Folders => &[],
        }
    }

    /// The filter as Everything search syntax, to put in front of the query ("" for All).
    pub fn everything(self) -> String {
        match self {
            Filter::All => String::new(),
            Filter::Folders => "folder: ".into(),
            other => format!("file: ext:{} ", other.extensions().join(";")),
        }
    }

    /// Whether an entry passes this filter.
    pub fn accepts(self, name: &str, is_folder: bool) -> bool {
        match self {
            Filter::All => true,
            Filter::Folders => is_folder,
            other => !is_folder && extension(name).is_some_and(|ext| other.extensions().iter().any(|e| e.eq_ignore_ascii_case(ext))),
        }
    }
}

/// The extension of a file name, without the dot ("tar.gz" gives "gz").
pub fn extension(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && !ext.contains(' ')).then_some(ext)
}

/// What kind of thing a name is, for the result's label.
pub fn kind_label(name: &str, is_folder: bool) -> &'static str {
    if is_folder {
        return "Folder";
    }
    for filter in [Filter::Apps, Filter::Documents, Filter::Pictures, Filter::Audio, Filter::Video, Filter::Code, Filter::Archives] {
        if filter.accepts(name, false) {
            return match filter {
                Filter::Apps => "App",
                Filter::Documents => "Document",
                Filter::Pictures => "Image",
                Filter::Audio => "Audio",
                Filter::Video => "Video",
                Filter::Code => "Code",
                _ => "Archive",
            };
        }
    }
    "File"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        assert!(Filter::Pictures.accepts("IMG_2041.HEIC", false));
        assert!(!Filter::Pictures.accepts("photos", true));
        assert!(Filter::Folders.accepts("photos", true));
        assert!(Filter::Code.accepts("main.rs", false));
        assert!(!Filter::Code.accepts("README", false));
        assert_eq!(Filter::All.everything(), "");
        assert_eq!(Filter::Folders.everything(), "folder: ");
        assert!(Filter::Audio.everything().starts_with("file: ext:mp3;wav"));
        assert_eq!(extension("archive.tar.gz"), Some("gz"));
        assert_eq!(extension(".gitignore"), None);
        assert_eq!(kind_label("Visual Studio Code.lnk", false), "App");
        assert_eq!(kind_label("notes", true), "Folder");
        assert_eq!(kind_label("report.pdf", false), "Document");
    }
}
