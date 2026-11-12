//! Whether what was typed names a file or describes one: "budget 2024" is a name, "a dog on the beach" a description,
//! and "that one photo with a cake in it" a description of an image. A description goes to the models that know what
//! images show and what documents mean; a name, to the names. Rules, not a model: they run on every keystroke, and a
//! name taken for a description costs little (images by what they show join the names), while the reverse is rare.
//! Measured on the searches in the tests below.

use crate::query::Terms;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// A name, or part of one: names first, as always.
    Name,
    /// A description: what fits by meaning joins the names more readily.
    Description,
    /// A description of an image ("a photo of …", "that screenshot with …"): images by what they show.
    Image,
}

/// Words that ask for an image.
const MEDIA: &[&str] = &["photo", "photos", "picture", "pictures", "pic", "pics", "image", "images", "screenshot", "screenshots",
                         "selfie", "selfies", "snapshot", "snapshots"];

/// Words that join a description's parts, rarely a file's name.
pub(crate) const JOINING: &[&str] = &["a", "an", "the", "of", "with", "in", "on", "at", "where", "from", "near", "my", "our", "his", "her",
                           "their", "some", "someone", "somebody", "people", "that", "this", "these", "those", "showing",
                           "shows", "wearing", "holding", "and", "over", "under", "behind", "front", "against", "by", "into",
                           "inside", "outside", "around", "while", "who", "which", "one", "we", "i", "me", "us", "it",
                           // What's said of a file's words: "the pdf that has my resume", "the one where he talks about…".
                           "has", "have", "having", "had", "contains", "containing", "contain", "mentions", "mentioning",
                           "mentioned", "about", "regarding", "called", "named", "titled", "said", "says", "talks", "talked",
                           "he", "she", "they", "you", "your", "its"];

/// Names of kinds of files: "pride and prejudice pdf" names a file.
const KINDS: &[&str] = &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "csv", "mp3", "mp4", "mkv", "zip", "rar", "exe",
                         "jpg", "jpeg", "png", "gif", "heic", "msi", "iso", "json", "md", "py", "rs", "js", "html", "folder"];

/// How `terms` reads. Everything's syntax, paths, regular expressions and anything with a digit or a file's extension
/// are names.
pub fn route(terms: &Terms) -> Route {
    if terms.syntax || terms.matching.regex || terms.matching.path {
        return Route::Name;
    }
    let text = terms.text.as_str();
    if text.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|', '_']) || text.chars().any(|c| c.is_ascii_digit()) {
        return Route::Name;
    }
    let words: Vec<String> = text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.').to_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    // "notes.txt", "setup.exe"
    if words.iter().any(|word| word.trim_end_matches('.').contains('.') || KINDS.contains(&word.as_str())) {
        return Route::Name;
    }
    if words.len() < 3 {
        return Route::Name;
    }
    let joining = words.iter().filter(|word| JOINING.contains(&word.as_str()) || is_doing(word)).count();
    if joining == 0 {
        return Route::Name;
    }
    if words.iter().any(|word| MEDIA.contains(&word.as_str())) {
        Route::Image
    } else {
        Route::Description
    }
}

/// "playing", "sitting": what's happening in it.
fn is_doing(word: &str) -> bool {
    word.len() > 5 && word.ends_with("ing") && !matches!(word, "building" | "buildings" | "meeting" | "meetings" | "wedding"
        | "weddings" | "recording" | "recordings" | "painting" | "paintings" | "drawing" | "drawings" | "setting" | "settings"
        | "listing" | "spring" | "morning" | "evening" | "clothing" | "something" | "nothing" | "everything" | "thing" | "things"
        | "housing" | "pudding" | "ceiling" | "earring" | "earrings" | "string" | "king" | "during" | "training" | "billing"
        | "banking" | "shopping" | "booking" | "bookings" | "heading" | "reading" | "writing" | "marketing" | "accounting" | "pricing")
}

/// A kind of file a description asks for: in so many words ("that photo of my cake", "the video where…", "that audio
/// of…"), or by what it says of it ("what he said about the budget"). What's of that kind comes first; the rest stays,
/// after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Document,
}

impl Kind {
    /// Whether a file by this name is of the kind.
    pub fn holds(self, name: &str) -> bool {
        let Some(extension) = crate::filter::extension(name) else { return false };
        let extension = extension.to_ascii_lowercase();
        let list = match self {
            Kind::Image => crate::filter::PICTURES,
            Kind::Video => crate::filter::VIDEO,
            Kind::Audio => crate::filter::AUDIO,
            Kind::Document => crate::filter::DOCUMENTS,
        };
        list.contains(&extension.as_str())
    }
}

/// Words that name a kind, in a few languages (Arabic by its stems, which take "the" and "and" in front).
pub(crate) fn named(word: &str) -> Option<Kind> {
    let kind = match word {
        "photo" | "photos" | "picture" | "pictures" | "pic" | "pics" | "image" | "images" | "screenshot" | "screenshots"
        | "selfie" | "selfies" | "snapshot" | "snapshots" | "foto" | "fotos" | "bild" | "bilder" | "imagen" | "imagenes" => Kind::Image,
        "video" | "videos" | "vid" | "vids" | "clip" | "clips" | "footage" | "movie" | "movies" | "film" | "films" | "reel"
        | "reels" | "vidéo" | "vidéos" | "vídeo" | "vídeos" => Kind::Video,
        "audio" | "audios" | "recording" | "recordings" | "recorded" | "voice" | "voicemail" | "voicemails" | "podcast"
        | "podcasts" | "song" | "songs" | "enregistrement" | "grabación" | "grabacion" | "aufnahme" => Kind::Audio,
        "document" | "documents" | "email" | "emails" | "letter" | "letters" | "report" | "reports" | "presentation"
        | "presentations" | "slides" | "spreadsheet" | "spreadsheets" | "essay" | "contract" | "invoice" | "invoices"
        | "dokument" | "documento" => Kind::Document,
        _ if word.contains("فيديو") || word.contains("فديو") => Kind::Video,
        _ if word.contains("صور") => Kind::Image,
        _ if word.contains("تسجيل") || word.contains("صوت") => Kind::Audio,
        _ if word.contains("مستند") || word.contains("رسال") => Kind::Document,
        _ => return None,
    };
    Some(kind)
}

/// Words that say something was spoken: a recording's, when no kind is named.
const SPOKEN: &[&str] = &["said", "says", "saying", "say", "talked", "talking", "talks", "mentioned", "mentions", "heard",
                          "explained", "explaining", "told", "telling"];

/// Whether `terms` may ask for a kind at all: a description, or text in another script of two words or more (whose
/// descriptions `route` doesn't know). A name like "photo editor" asks for nothing.
pub fn describes(terms: &Terms) -> bool {
    let text = terms.text.to_lowercase();
    let words = text.split_whitespace().filter(|word| word.chars().any(char::is_alphanumeric)).count();
    let foreign = text.chars().any(|c| c.is_alphabetic() && !c.is_ascii());
    route(terms) != Route::Name || (foreign && words >= 2 && !terms.syntax && !terms.matching.regex)
}

/// The kind of file `terms` asks for, if it asks for one. Only for descriptions (a name like "photo editor" asks for
/// nothing), and for text in other scripts, whose descriptions the rules above don't know.
pub fn wants(terms: &Terms) -> Option<Kind> {
    if !describes(terms) {
        return None;
    }
    let text = terms.text.to_lowercase();
    let words: Vec<&str> = text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .collect();
    let route = route(terms);
    if text.contains("screen recording") || text.contains("screen capture") {
        return Some(Kind::Video);
    }
    // The first named wins: "the video where he said…" is a video.
    if let Some(kind) = words.iter().find_map(|word| named(word)) {
        return Some(kind);
    }
    if route != Route::Name && words.iter().any(|word| SPOKEN.contains(word)) {
        return Some(Kind::Audio);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(text: &str) -> Route {
        route(&Terms::parse(text))
    }

    /// 50 searches for a file by its name and 50 descriptions, written alongside the rules: a check that they
    /// do what's meant, not a measure of what people type.
    #[test]
    fn names_and_descriptions() {
        let names = [
            "budget", "invoice", "resume", "cv 2024", "tax return 2023", "IMG_2041", "DSC0012", "Screenshot 2024-05-01 101500",
            "meeting notes", "project proposal final", "report q3", "lecture 5 slides", "thesis draft v2", "passport scan",
            "chrome", "visual studio code", "vsc", "photoshop", "steam", "league of legends", "minecraft", "notes.txt",
            "main.rs", "package.json", "readme", "family photos", "wedding photos", "vacation 2019", "holiday pics", "excavate",
            "downloads", "budget 2024.xlsx", "lease agreement", "bank statement march", "insurance policy", "the lord of the rings",
            "game of thrones s01e01", "pride and prejudice pdf", "song.mp3", "lecture recording", "zoom recording",
            "cover letter google", "w2 2023", "birth certificate", "car insurance card", "linear algebra homework 3",
            "chapter 4 notes", "setup.exe", "wedding photos italy", "kitchen renovation quote",
        ];
        let descriptions = [
            "that one photo with a cake in it", "a dog on the beach", "guy in a green shirt", "my mom at the wedding",
            "the picture of the sunset over the lake", "screenshot of a chat with my boss", "a receipt from the restaurant",
            "people sitting around a table", "a man riding a horse", "kids playing in the snow",
            "the photo where we were at the eiffel tower", "a cat sleeping on the sofa", "snowy mountains with a lake",
            "a whiteboard with diagrams", "a plate of pasta", "that image with the red car", "the one with the birthday cake",
            "a selfie in front of a mirror", "my handwritten notes about calculus", "a screenshot of an error message",
            "photo of a bridge at night", "a group photo at graduation", "dog wearing a hat", "two people hiking in the forest",
            "a flyer for the concert", "the screenshot showing my flight booking", "picture of a pie", "sunset at the beach",
            "a crowded street market", "the photo i took of the northern lights", "an old building with columns",
            "a cup of coffee on a desk", "map of the city", "a bowl of ramen", "a baby smiling", "someone holding a guitar",
            "a diagram of a neural network", "a car parked in front of a house", "the image where the text is blurry",
            "food on a table", "a man in a suit giving a speech", "pictures from the hike with mountains",
            "a document with a signature", "a chart showing sales", "flowers in a vase", "that photo with the big tree",
            "a laptop on a bed", "people dancing at a party", "a red bicycle against a wall", "a boat on the river",
        ];
        let has_media = |text: &str| text.split_whitespace().any(|word| MEDIA.contains(&word));
        // A name taken for an image's description shows images instead of it: none may be.
        let mut costly = Vec::new();
        let mut cheap = Vec::new();
        for name in names {
            match of(name) {
                Route::Image => costly.push(name),
                Route::Description => cheap.push(name),
                Route::Name => {}
            }
        }
        let mut missed = Vec::new();
        for description in descriptions {
            let wanted = if has_media(description) { Route::Image } else { Route::Description };
            if of(description) != wanted {
                missed.push((description, of(description)));
            }
        }
        println!("names read as descriptions (images join them): {cheap:?}");
        println!("descriptions missed: {missed:?}");
        assert!(costly.is_empty(), "names read as images' descriptions: {costly:?}");
        assert!(cheap.len() <= 3, "{cheap:?}");
        assert!(missed.len() <= 3, "{missed:?}");
    }

    #[test]
    fn syntax_and_paths_are_names() {
        assert_eq!(of("ext:jpg a dog"), Route::Name);
        assert_eq!(of(r"C:\Users\a dog on the beach"), Route::Name);
        assert_eq!(of("a dog on the beach"), Route::Description);
        assert_eq!(of("photo of a dog"), Route::Image);
    }
    #[test]
    fn kinds_asked_for() {
        let wants = |text: &str| wants(&Terms::parse(text));
        assert_eq!(wants("that photo of my cake"), Some(Kind::Image));
        assert_eq!(wants("the picture of the sunset over the lake"), Some(Kind::Image));
        assert_eq!(wants("that audio of my brother singing"), Some(Kind::Audio));
        assert_eq!(wants("the recording of the meeting with the bank"), Some(Kind::Audio));
        assert_eq!(wants("what he said about the budget"), Some(Kind::Audio));
        assert_eq!(wants("the video where he said hello"), Some(Kind::Video));
        assert_eq!(wants("a clip of my dog at the beach"), Some(Kind::Video));
        assert_eq!(wants("my screen recording of the bug"), Some(Kind::Video));
        assert_eq!(wants("the email from my landlord about the rent"), Some(Kind::Document));
        assert_eq!(wants("صورة الكيكة"), Some(Kind::Image));
        assert_eq!(wants("فيديو عيد الميلاد"), Some(Kind::Video));
        assert_eq!(wants("a dog on the beach"), None);
        // Names ask for nothing.
        assert_eq!(wants("photo editor"), None);
        assert_eq!(wants("lecture recording"), None);
        assert_eq!(wants("song.mp3"), None);
        assert!(Kind::Video.holds("clip.MP4") && Kind::Image.holds("a.heic") && !Kind::Image.holds("a.mp4"));
    }
}
