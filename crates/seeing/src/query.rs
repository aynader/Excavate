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

/// Days since 1970 from a civil date (Howard Hinnant's algorithm); months past 12 roll into the next year.
pub fn days(year: i64, month: i64, day: i64) -> i64 {
    let (year, month) = (year + (month - 1).div_euclid(12), (month - 1).rem_euclid(12) + 1);
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * m + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

/// The civil date of a day since 1970: (year, month, day).
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let m = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * m + 2) / 5 + 1;
    let month = if m < 10 { m + 3 } else { m - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
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
            (season, _) if season_of(season).is_some() => {
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
            (month, _) if month_of(month).is_some() && (next.and_then(year_of).is_some() || month.len() > 3 || index > 0) => {
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

/// Words that ask for an image rather than say what's in it, before it: "find", "show me", "that one".
const ASKING: &[&str] = &["find", "search", "for", "show", "me", "get", "give", "looking", "look", "i", "im", "want", "need", "wheres",
                          "the", "that", "this", "those", "these", "one", "ones", "some", "my", "a", "an", "of", "with"];
/// What a description asks for, as the models' own captions name it.
fn medium(word: &str) -> Option<&'static str> {
    match word {
        "photo" | "photos" | "picture" | "pictures" | "pic" | "pics" | "image" | "images" | "snapshot" | "snapshots" => Some("photo"),
        "screenshot" | "screenshots" | "screengrab" => Some("screenshot"),
        _ => None,
    }
}
/// What joins the medium to what it shows: "a photo with …", "the picture where …".
const JOINING: &[&str] = &["with", "of", "where", "showing", "shows", "has", "having", "containing", "contains", "featuring", "that", "which", "in", "when", "from"];
/// Said after, and saying nothing about it: "… in it", "… or something".
const AFTER: &[&[&str]] = &[&["in", "it"], &["in", "them"], &["on", "it"], &["or", "something"], &["i", "think"], &["somewhere"], &["please"]];

/// A description as the models compare it best: what asks for an image taken out ("that one photo with a cake in it" is "a
/// photo of a cake"). As typed when there's nothing of the kind, or nothing would be left.
pub fn tidy(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let plain = |word: &str| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase().replace('\'', "");
    let mut start = 0;
    let mut medium_of = None;
    while start < words.len() {
        let word = plain(words[start]);
        if let Some(found) = medium(&word) {
            medium_of = Some(found);
            start += 1;
            // "… photo with/where/that shows …"
            while start < words.len() && JOINING.contains(&plain(words[start]).as_str()) {
                start += 1;
            }
            break;
        }
        if !ASKING.contains(&word.as_str()) {
            // What it shows begins here, with its article ("… with a guy" is "a guy").
            if start > 0 && matches!(plain(words[start - 1]).as_str(), "a" | "an") {
                start -= 1;
            }
            break;
        }
        start += 1;
    }
    let mut end = words.len();
    'after: loop {
        for tail in AFTER {
            if end >= start + tail.len() + 1 && words[end - tail.len()..end].iter().map(|word| plain(word)).eq(tail.iter().map(|word| word.to_string())) {
                end -= tail.len();
                continue 'after;
            }
        }
        break;
    }
    if start == 0 && end == words.len() {
        return text.trim().to_string();
    }
    if start >= end {
        return text.trim().to_string();
    }
    let rest = words[start..end].join(" ");
    let rest = rest.trim_end_matches(['.', '?', '!', ',']);
    match medium_of {
        Some(medium) => format!("a {medium} of {rest}"),
        None => rest.to_string(),
    }
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
