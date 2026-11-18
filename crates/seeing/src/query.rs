//! Reading a description for when and where: "dog in Prague last winter" is a dog (for the models), Prague (from the
//! photos' positions) and December to February (from when they were taken). The models can't know either from pixels.

/// What a description says, apart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Understood {
    /// What's left for the models; empty when the description was only a time or a place.
    pub rest: String,
    /// When: from (included) and to (not), in the photos' local reckoning (`facts::Fact::when`).
    pub when: Option<(i64, i64)>,
    /// Where: a place's name, lowercase, as `facts::Facts::place_names` has them.
    pub place: Option<String>,
}

const DAY: i64 = 86_400;

pub use common::civil;

/// Days since 1970 from a civil date; months past 12 roll into the next year.
pub fn days(year: i64, month: i64, day: i64) -> i64 {
    let (year, month) = (year + (month - 1).div_euclid(12), (month - 1).rem_euclid(12) + 1);
    common::days_from_civil(year, month, day)
}

fn month_range(year: i64, month: i64, months: i64) -> (i64, i64) {
    (days(year, month, 1) * DAY, days(year, month + months, 1) * DAY)
}

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

fn month_of(word: &str) -> Option<i64> {
    MONTHS.iter().position(|month| *month == word || (word.len() >= 3 && word != "may" && month.starts_with(word) && word.len() <= month.len() && word.len() == 3))
        .map(|index| index as i64 + 1)
}

/// The first month of a season (the northern hemisphere's): spring, summer, autumn; winter starts in the December
/// before its year.
fn season_of(word: &str) -> Option<i64> {
    match word {
        "spring" => Some(3),
        "summer" => Some(6),
        "autumn" | "fall" => Some(9),
        "winter" => Some(12),
        _ => None,
    }
}

fn year_of(word: &str) -> Option<i64> {
    word.parse::<i64>().ok().filter(|year| (1970..=2100).contains(year) && word.len() == 4)
}

/// Words that only say "images" or join the others: nothing for the models once a time or place is taken out.
const EMPTY: &[&str] = &["photo", "photos", "picture", "pictures", "pic", "pics", "image", "images", "taken", "shot", "from", "in", "at",
                         "during", "of", "my", "the", "near", "on", "around"];

/// Takes the time and place out of `text`. `now` is the present in the photos' local reckoning; `places` every place's
/// name the photos have (lowercase).
pub fn understand(text: &str, now: i64, places: &std::collections::HashSet<String>) -> Understood {
    let words: Vec<String> = text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|word| !word.is_empty())
        .collect();
    let mut used = vec![false; words.len()];
    let (year_now, month_now, day_now) = civil(now.div_euclid(DAY));
    let today = days(year_now, month_now, day_now) * DAY;
    let mut when = None;
    let mut index = 0;
    while index < words.len() && when.is_none() {
        let word = words[index].as_str();
        let next = words.get(index + 1).map(String::as_str);
        // "fall", "spring", "march" are words before they're times ("about to fall", "people march"): times after "in",
        // "during", "early"… or before a year.
        let timely = !matches!(word, "fall" | "spring" | "march") || next.and_then(year_of).is_some()
            || index.checked_sub(1).is_some_and(|before| matches!(words[before].as_str(), "in" | "during" | "of" | "early" | "late" | "mid" | "since" | "from"));
        let mut take = |count: usize, range: (i64, i64)| {
            for slot in used.iter_mut().skip(index).take(count) {
                *slot = true;
            }
            when = Some(range);
        };
        match (word, next) {
            ("today", _) => take(1, (today, today + DAY)),
            ("yesterday", _) => take(1, (today - DAY, today)),
            ("this" | "last" | "past", Some("week")) => {
                let end = if word == "last" { today - 6 * DAY } else { today + DAY };
                take(2, (end - 7 * DAY, end));
            }
            ("this", Some("month")) => take(2, month_range(year_now, month_now, 1)),
            ("last" | "past", Some("month")) => take(2, month_range(year_now, month_now - 1, 1)),
            ("this", Some("year")) => take(2, month_range(year_now, 1, 12)),
            ("last" | "past", Some("year")) => take(2, month_range(year_now - 1, 1, 12)),
            ("this" | "last" | "past", Some(season)) if season_of(season).is_some() => {
                let first = season_of(season).unwrap();
                // The latest of these seasons to have started, and for "last" the latest to have ended.
                let mut year = year_now;
                let start = |year: i64| if first == 12 { days(year - 1, 12, 1) * DAY } else { days(year, first, 1) * DAY };
                while start(year) > now {
                    year -= 1;
                }
                if word != "this" && start(year) + 92 * DAY > now {
                    year -= 1;
                }
                let begins = start(year);
                take(2, (begins, if first == 12 { days(year, 3, 1) * DAY } else { days(year, first + 3, 1) * DAY }));
            }
            (season, _) if season_of(season).is_some() && timely => {
                let first = season_of(season).unwrap();
                let named = next.and_then(year_of);
                let mut year = named.unwrap_or(year_now);
                let start = |year: i64| if first == 12 { days(year - 1, 12, 1) * DAY } else { days(year, first, 1) * DAY };
                if named.is_none() {
                    while start(year) > now {
                        year -= 1;
                    }
                }
                let begins = start(year);
                take(if named.is_some() { 2 } else { 1 }, (begins, if first == 12 { days(year, 3, 1) * DAY } else { days(year, first + 3, 1) * DAY }));
            }
            (month, _) if month_of(month).is_some() && timely && (next.and_then(year_of).is_some() || month.len() > 3 || index > 0) => {
                let month = month_of(month).unwrap();
                let named = next.and_then(year_of);
                let year = named.unwrap_or(if month > month_now { year_now - 1 } else { year_now });
                take(if named.is_some() { 2 } else { 1 }, month_range(year, month, 1));
            }
            (year, _) if year_of(year).is_some() => take(1, month_range(year_of(year).unwrap(), 1, 12)),
            _ => {}
        }
        index += 1;
    }
    // A place: the longest run of words that names one.
    let mut place = None;
    'search: for length in (1..=3).rev() {
        for start in 0..words.len().saturating_sub(length - 1) {
            if used[start..start + length].iter().any(|taken| *taken) {
                continue;
            }
            let name = words[start..start + length].join(" ");
            if places.contains(&name) {
                for slot in &mut used[start..start + length] {
                    *slot = true;
                }
                place = Some(name);
                break 'search;
            }
        }
    }
    let rest: Vec<&str> = words.iter().zip(&used).filter(|(_, used)| !**used).map(|(word, _)| word.as_str()).collect();
    let something = rest.iter().any(|word| !EMPTY.contains(word));
    // Untouched, the description goes to the models as typed.
    let rest = if when.is_none() && place.is_none() { text.trim().to_string() } else if something { rest.join(" ") } else { String::new() };
    Understood { rest, when, place }
}

/// Words that ask for an image rather than say what's in it, before it: "find", "show me", "where is that one", "can you
/// find the".
const ASKING: &[&str] = &["find", "search", "for", "show", "me", "get", "give", "looking", "look", "i", "im", "want", "need", "wheres",
                          "the", "that", "this", "those", "these", "one", "ones", "some", "my", "our", "a", "an", "of", "with",
                          "where", "is", "was", "can", "could", "would", "you", "please", "pls", "plz", "help", "did", "do", "to",
                          "see", "open", "pull", "up", "bring", "all", "any", "there", "theres", "its", "remember", "trying",
                          "try", "taking", "id", "like", "us", "again", "hey", "somewhere", "have", "put", "had", "got"];

/// What a description asks for, as the models' own captions name it; a letter off too ("picutre", "phot").
fn medium(word: &str) -> Option<&'static str> {
    match word {
        "photo" | "photos" | "picture" | "pictures" | "pic" | "pics" | "image" | "images" | "snapshot" | "snapshots" | "photograph"
        | "photographs" | "foto" | "fotos" => Some("photo"),
        "screenshot" | "screenshots" | "screengrab" | "screengrabs" => Some("screenshot"),
        // Words of their own, a letter off one.
        "pictured" | "imaged" | "imagen" | "photon" | "photons" | "mage" | "mages" => None,
        _ if word.len() >= 4 && ["photo", "photos", "picture", "pictures", "image", "images"].iter().any(|medium| near(word, medium)) => Some("photo"),
        _ if word.len() >= 9 && ["screenshot", "screenshots"].iter().any(|medium| near(word, medium)) => Some("screenshot"),
        _ => None,
    }
}

/// Whether two different words are a letter apart: one changed, added or left out, or two side by side swapped.
pub fn near(word: &str, other: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (word.chars().collect(), other.chars().collect());
    if a == b || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let same_start = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let same_end = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count().min(a.len().min(b.len()) - same_start);
    match (a.len() - same_start - same_end, b.len() - same_start - same_end) {
        (1, 1) | (1, 0) | (0, 1) => true,
        (2, 2) => a[same_start] == b[same_start + 1] && a[same_start + 1] == b[same_start],
        _ => false,
    }
}

/// What joins the medium to what it shows: "a photo with …", "the picture where …".
const JOINING: &[&str] = &["with", "of", "where", "showing", "shows", "has", "having", "containing", "contains", "featuring", "that", "which", "when"];
/// Where it was taken, kept after the medium: "a photo at the beach", "a picture in the snow".
const PLACING: &[&str] = &["at", "on", "in", "from", "near", "during", "inside", "outside", "under"];
/// What someone did with an image, said of it: "the photo i took of …", "a pic my mom sent me".
const DONE: &[&str] = &["took", "take", "taken", "shot", "snapped", "captured", "saved", "save", "downloaded", "download", "got", "made",
                        "sent", "posted", "shared", "uploaded", "received", "found", "edited", "scanned", "printed", "liked",
                        "screenshotted", "think", "remember", "had", "have", "kept", "showed"];
/// The same, said of the image alone: "a photo taken at the beach", "the pic sent by my sister".
const DONE_TO: &[&str] = &["taken", "shot", "snapped", "captured", "saved", "downloaded", "sent", "posted", "shared", "uploaded", "received",
                           "scanned", "printed", "edited", "screenshotted"];
/// Who did it: "i", "we", "my mom", "my friend".
const DOERS: &[&str] = &["i", "we", "you", "he", "she", "they", "someone", "somebody", "my", "our", "his", "her", "their", "ive", "id", "im"];
/// Words that can't be part of who did something.
const NOT_DOERS: &[&str] = &["a", "an", "the", "of", "with", "where", "in", "on", "at", "from", "and", "or", "that", "which", "who", "is", "are",
                             "was", "were", "to", "for", "by", "near", "some", "this", "these", "those", "it"];
/// Where it's kept, said of it: "the photo somewhere on my computer of …".
const KEPT: &[&str] = &["computer", "pc", "laptop", "phone", "drive", "desktop", "files", "folder", "downloads", "gallery", "camera", "icloud", "onedrive"];
/// Said after, and saying nothing about it: "… in it", "… please".
const AFTER: &[&[&str]] = &[&["in", "it"], &["in", "them"], &["on", "it"], &["or", "something"], &["i", "think"], &["somewhere"], &["please"],
                            &["pls"], &["plz"], &["thanks"], &["thx"], &["thank", "you"], &["for", "me"], &["again"]];
/// What a request after a comma is made of: "…, can you find it".
const REQUEST: &[&str] = &["can", "could", "would", "you", "please", "find", "it", "show", "me", "get", "thanks", "thank", "pls", "help",
                           "i", "need", "want", "where", "is", "that", "one", "for", "the", "picture", "photo", "pic", "image"];

/// Where a description goes on after what's said of the image starting at `at` ("i took", "my mom sent me", "that i
/// saved", "taken by me"), or None when that isn't there. At most two words for who did it ("my mom", "john").
fn after_done(words: &[String], at: usize, end: usize) -> Option<usize> {
    let mut at = at;
    if at < end && matches!(words[at].as_str(), "that" | "which") {
        at += 1;
    }
    let mut past = None;
    if at < end && DONE_TO.contains(&words[at].as_str()) {
        past = Some(at + 1);
        // "… by me", "… by my sister"
        if at + 2 < end && words[at + 1] == "by" {
            past = Some(if matches!(words[at + 2].as_str(), "my" | "our") { (at + 4).min(end) } else { at + 3 });
        }
    } else {
        for verb in at + 1..end.min(at + 3) {
            if DONE.contains(&words[verb].as_str()) {
                let doers = &words[at..verb];
                if (DOERS.contains(&doers[0].as_str()) || doers.len() == 1)
                    && doers.iter().all(|word| !NOT_DOERS.contains(&word.as_str()) && medium(word).is_none() && !DONE.contains(&word.as_str())) {
                    past = Some(verb + 1);
                }
                break;
            }
        }
    }
    let mut past = past?;
    // "… sent me", "… took for me", "i think i took"
    loop {
        if past < end && matches!(words[past].as_str(), "me" | "us" | "it") {
            past += 1;
        } else if past + 1 < end && (matches!(words[past].as_str(), "to" | "for") && matches!(words[past + 1].as_str(), "me" | "us")
            || DOERS.contains(&words[past].as_str()) && DONE.contains(&words[past + 1].as_str())) {
            past += 2;
        } else {
            break;
        }
    }
    Some(past)
}

/// A description as the models compare it best: what asks for an image taken out ("that one photo with a cake in it" is "a
/// photo of a cake"), and what's said of the image rather than of what it shows ("a photo i took of a card" is "a photo of
/// a card", "pic of the beach my mom sent me" is "a photo of the beach"). As typed when there's nothing of the kind, or
/// nothing would be left.
pub fn tidy(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let plain: Vec<String> = words.iter()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase().replace(['\'', '\u{2019}'], ""))
        .collect();
    let mut end = words.len();
    // "…, can you find it"
    if let Some(comma) = words.iter().rposition(|word| word.ends_with(','))
        && comma + 1 < end && plain[comma + 1..end].iter().all(|word| REQUEST.contains(&word.as_str())) {
            end = comma + 1;
        }
    let mut start = 0;
    let mut medium_of = None;
    let mut placed = false;
    while start < end {
        let word = plain[start].as_str();
        // "screen grab", "screen shot"
        let two = start + 1 < end && word == "screen" && matches!(plain[start + 1].as_str(), "grab" | "shot" | "capture" | "grabs" | "shots");
        if let Some(found) = if two { Some("screenshot") } else { medium(word) } {
            medium_of = Some(found);
            start += if two { 2 } else { 1 };
            // Once past "of", "with"…, what the image shows has begun ("a photo of zebras made of lego").
            let mut joined = false;
            loop {
                // "… photo i took of …", "… pic my mom sent me of …"
                if let Some(past) = after_done(&plain, start, end).filter(|_| !joined || DOERS.contains(&plain[start].as_str())) {
                    start = past;
                    continue;
                }
                // "… photo somewhere on my computer of …"
                if start < end && plain[start] == "somewhere" {
                    start += 1;
                    continue;
                }
                if start + 2 < end && matches!(plain[start].as_str(), "on" | "in") && matches!(plain[start + 1].as_str(), "my" | "the" | "our")
                    && KEPT.contains(&plain[start + 2].as_str()) {
                    start += 3;
                    continue;
                }
                // "… photo with/where/that shows …", "… where there were …"
                let joining = start < end && (JOINING.contains(&plain[start].as_str())
                    || (matches!(plain[start].as_str(), "there" | "theres") && JOINING.contains(&plain[start - 1].as_str()))
                    || (matches!(plain[start].as_str(), "is" | "are" | "was" | "were") && plain[start - 1] == "there"));
                if joining {
                    start += 1;
                    joined = true;
                    continue;
                }
                break;
            }
            // "… photo at the beach": where, kept.
            placed = start < end && PLACING.contains(&plain[start].as_str());
            break;
        }
        if !ASKING.contains(&word) {
            // What it shows begins here, with its article ("… with a guy" is "a guy").
            if start > 0 && matches!(plain[start - 1].as_str(), "a" | "an") {
                start -= 1;
            }
            break;
        }
        start += 1;
    }
    // "… i saved", "… that my mom sent me", "… taken by me": what's said of the image, after what it shows.
    let said_of_it = (start + 1..end).find(|&at| {
        let word = plain[at].as_str();
        let begins = match word {
            // (Not "a dog that got wet": someone after it.)
            "that" | "which" => at + 1 < end && DOERS.contains(&plain[at + 1].as_str()),
            _ if DONE_TO.contains(&word) => at + 1 < end && plain[at + 1] == "by",
            _ => DOERS.contains(&word),
        };
        begins && after_done(&plain, at, end).is_some()
    });
    if let Some(at) = said_of_it {
        end = at;
    }
    'after: loop {
        for tail in AFTER {
            if end > start + tail.len() && plain[end - tail.len()..end].iter().map(String::as_str).eq(tail.iter().copied()) {
                end -= tail.len();
                continue 'after;
            }
        }
        // "… on my computer"
        if end >= start + 4 && matches!(plain[end - 3].as_str(), "on" | "in") && matches!(plain[end - 2].as_str(), "my" | "the" | "our")
            && KEPT.contains(&plain[end - 1].as_str()) {
            end -= 3;
            continue;
        }
        break;
    }
    if (start == 0 && end == words.len()) || start >= end {
        return text.trim().to_string();
    }
    let rest = words[start..end].join(" ");
    let rest = rest.trim_end_matches(['.', '?', '!', ',']);
    match medium_of {
        Some(medium) if placed => format!("a {medium} {rest}"),
        Some(medium) => format!("a {medium} of {rest}"),
        None => rest.to_string(),
    }
}

/// The kinds `kind_of` tells apart, in its weights' order.
pub const KINDS: [&str; 5] = ["image", "video", "audio", "document", "none"];

/// A linear classifier on EmbeddingGemma 2's vector of a description (as the search helper makes it: Excavate's query
/// prompt, unit length, 768 numbers), trained on labelled descriptions: 5 × 768 weights and 5 biases, little-endian floats.
const KIND_WEIGHTS: &[u8] = include_bytes!("kinds.bin");

/// The kind a description asks for ("image", "video", "audio", "document", or "none"), and how sure it is (0 to 1).
/// Measured on a held-out set of 198 descriptions: with the rules, overruling them when at least 0.8
/// sure (0.99 for what the rules take for a name), right 92.9% of the time against the rules' 73.7%.
pub fn kind_of(vector: &[f32]) -> Option<(&'static str, f32)> {
    const DIMS: usize = 768;
    if vector.len() != DIMS || KIND_WEIGHTS.len() != (KINDS.len() * DIMS + KINDS.len()) * 4 {
        return None;
    }
    let number = |at: usize| f32::from_le_bytes([KIND_WEIGHTS[at * 4], KIND_WEIGHTS[at * 4 + 1], KIND_WEIGHTS[at * 4 + 2], KIND_WEIGHTS[at * 4 + 3]]);
    let logits: Vec<f32> = (0..KINDS.len())
        .map(|kind| (0..DIMS).map(|d| number(kind * DIMS + d) * vector[d]).sum::<f32>() + number(KINDS.len() * DIMS + kind))
        .collect();
    let top = logits.iter().copied().fold(f32::MIN, f32::max);
    let exps: Vec<f32> = logits.iter().map(|logit| (logit - top).exp()).collect();
    let total: f32 = exps.iter().sum();
    let (best, &most) = exps.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    Some((KINDS[best], most / total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_taken_out() {
        assert_eq!(tidy("that one photo with a cake in it"), "a photo of a cake");
        assert_eq!(tidy("find the picture where my dog sleeps on the sofa"), "a photo of my dog sleeps on the sofa");
        assert_eq!(tidy("show me screenshots of the bank app"), "a screenshot of the bank app");
        assert_eq!(tidy("the one with a guy in a green shirt"), "a guy in a green shirt");
        assert_eq!(tidy("pie"), "pie");
        assert_eq!(tidy("a man in a green shirt"), "a man in a green shirt");
        // Nothing would be left: as typed.
        assert_eq!(tidy("that one photo"), "that one photo");
        assert_eq!(tidy("photos"), "photos");
    }

    #[test]
    fn what_is_said_of_the_image_taken_out() {
        // Once "a photo of i took of a card": photos of people came first.
        assert_eq!(tidy("a photo i took of a card"), "a photo of a card");
        assert_eq!(tidy("the photo my mom sent me of the zebras in the forest"), "a photo of the zebras in the forest");
        assert_eq!(tidy("that pic john sent of the guy surrounded by fast food"), "a photo of the guy surrounded by fast food");
        assert_eq!(tidy("photo that my friend sent me of people kitesurfing"), "a photo of people kitesurfing");
        assert_eq!(tidy("pic of a cat in a pile of stuffed animals i saved"), "a photo of a cat in a pile of stuffed animals");
        assert_eq!(tidy("image of a white truck carrying a red car that i downloaded"), "a photo of a white truck carrying a red car");
        assert_eq!(tidy("photos of food i made"), "a photo of food");
        assert_eq!(tidy("photos i took at the tennis match"), "a photo at the tennis match");
        assert_eq!(tidy("a photo taken at the beach"), "a photo at the beach");
        assert_eq!(tidy("the photo of the zebras taken by my dad"), "a photo of the zebras");
        // Asking, before and after.
        assert_eq!(tidy("where is that photo of the train passing the brick building"), "a photo of the train passing the brick building");
        assert_eq!(tidy("can you find the photo of the motorcycles parked on the road"), "a photo of the motorcycles parked on the road");
        assert_eq!(tidy("could you find the image of the bus with palm trees thanks"), "a photo of the bus with palm trees");
        assert_eq!(tidy("show me the photo of a woman cooking eggs on the stove, thank you"), "a photo of a woman cooking eggs on the stove");
        assert_eq!(tidy("i remember taking this one picture where there were two dogs on the grass, can you find it"), "a photo of two dogs on the grass");
        assert_eq!(tidy("there was a photo somewhere on my computer of a lady in a yellow shirt"), "a photo of a lady in a yellow shirt");
        assert_eq!(tidy("photos with a laptop in them"), "a photo of a laptop");
        // A letter off the medium; two words for it.
        assert_eq!(tidy("picutre of two zebras in the woods"), "a photo of two zebras in the woods");
        assert_eq!(tidy("that screen grab of a train station platform"), "a screenshot of a train station platform");
        // What the image shows, doing something, stays.
        assert_eq!(tidy("a dog that got wet"), "a dog that got wet");
        assert_eq!(tidy("photo of zebras made of lego"), "a photo of zebras made of lego");
        assert_eq!(tidy("the picture where i took the kids to the zoo"), "a photo of the kids to the zoo");
        assert_eq!(tidy("a man taking a picture of the city"), "a man taking a picture of the city");
        assert_eq!(tidy("dog beach"), "dog beach");
        assert!(near("phot", "photo") && near("picutre", "picture") && near("imgae", "image"));
        assert!(!near("photo", "photo") && !near("plot", "photo") && medium("pictured").is_none());
    }

    #[test]
    fn calendar() {
        assert_eq!(days(1970, 1, 1), 0);
        assert_eq!(civil(days(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(days(2026, 13, 1), days(2027, 1, 1));
        assert_eq!(days(2026, 0, 1), days(2025, 12, 1));
    }

    #[test]
    fn times_and_places_taken_out() {
        // 3 November 2026, noon.
        let now = days(2026, 11, 3) * DAY + 12 * 3600;
        let places: std::collections::HashSet<String> = ["prague", "czechia", "new york"].into_iter().map(String::from).collect();
        let at = |y, m, d| days(y, m, d) * DAY;

        let understood = understand("dog in Prague last winter", now, &places);
        assert_eq!(understood.rest, "dog in");
        assert_eq!(understood.place.as_deref(), Some("prague"));
        assert_eq!(understood.when, Some((at(2025, 12, 1), at(2026, 3, 1))));

        assert_eq!(understand("photos from 2023", now, &places), Understood { rest: String::new(), when: Some((at(2023, 1, 1), at(2024, 1, 1))), place: None });
        assert_eq!(understand("beach in July", now, &places).when, Some((at(2026, 7, 1), at(2026, 8, 1))));
        assert_eq!(understand("snow december 2024", now, &places).when, Some((at(2024, 12, 1), at(2025, 1, 1))));
        // December hasn't come yet this year: the last one.
        assert_eq!(understand("party in december", now, &places).when, Some((at(2025, 12, 1), at(2026, 1, 1))));
        assert_eq!(understand("yesterday", now, &places).when, Some((at(2026, 11, 2), at(2026, 11, 3))));
        assert_eq!(understand("last month", now, &places).when, Some((at(2026, 10, 1), at(2026, 11, 1))));
        assert_eq!(understand("summer", now, &places).when, Some((at(2026, 6, 1), at(2026, 9, 1))));
        assert_eq!(understand("this summer", now, &places).when, Some((at(2026, 6, 1), at(2026, 9, 1))));
        assert_eq!(understand("last summer", now, &places).when, Some((at(2026, 6, 1), at(2026, 9, 1))));
        assert_eq!(understand("New York skyline", now, &places).place.as_deref(), Some("new york"));
        // Nothing of either: as typed.
        assert_eq!(understand("a man in a green shirt", now, &places), Understood { rest: "a man in a green shirt".into(), when: None, place: None });
        // "may" is a word before it's a month; a bare number isn't a year.
        assert_eq!(understand("may I see", now, &places).when, None);
        assert_eq!(understand("route 66", now, &places).when, None);
        // Words before times.
        assert_eq!(understand("a skateboarder about to fall", now, &places).when, None);
        assert_eq!(understand("people march down the street", now, &places).when, None);
        assert_eq!(understand("a metal spring", now, &places).when, None);
        assert_eq!(understand("leaves in fall", now, &places).when, Some((at(2026, 9, 1), at(2026, 12, 1))));
        assert_eq!(understand("snow in march", now, &places).when, Some((at(2026, 3, 1), at(2026, 4, 1))));
        assert_eq!(understand("march 2024", now, &places).when, Some((at(2024, 3, 1), at(2024, 4, 1))));
    }
}
