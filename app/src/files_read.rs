//! Activity's Files read card: how many files were read today, this past week, month and year, a bar an hour, day or
//! month, and the average, as a phone's health app shows steps.

use common::grouped as group;

/// One choice of the card's menu (Today, Past week, Past month, Past year).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    /// "Past week".
    pub choice: &'static str,
    /// Above the big number: "Average per day", "Today so far".
    pub kind: &'static str,
    /// "19,189".
    pub big: String,
    /// "Nov 12 to 18".
    pub when: String,
    pub total: String,
    /// The busiest bar's count, and what a bar is ("day").
    pub busiest: String,
    pub unit: &'static str,
    /// Images looked at and recordings heard.
    pub seen: String,
    /// The average as a share of the busiest bar, or -1 for none.
    pub average: f32,
    pub bars: Vec<Bar>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bar {
    /// Its height, as a share of the busiest bar (0 to 1).
    pub share: f32,
    /// Under it, on some bars only: "Mon", "Oct 20", "6 AM".
    pub label: String,
    /// When it is picked: "Mon, Nov 16".
    pub name: String,
    pub read: String,
    pub seen: String,
}

/// Counts by local day (days since 1970): (files read, seen or heard).
pub type Days = Vec<(i64, u64, u64)>;

/// The past year's days from the catalog (read-only), counted by the local day each was last read on.
pub fn days(catalog: &std::path::Path, today: i64, offset: i64) -> Days {
    use rusqlite::OpenFlags;
    let mut days: Days = Vec::new();
    let Ok(db) = rusqlite::Connection::open_with_flags(catalog, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
        return days;
    };
    let from = (today - 400) * 86_400 - offset;
    let mut count = |sql: &str, seen: bool| {
        let Ok(mut statement) = db.prepare(sql) else { return };
        let rows = statement.query_map([offset, from], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)));
        for (day, n) in rows.into_iter().flatten().flatten() {
            let n = n.max(0) as u64;
            match days.iter_mut().find(|(d, _, _)| *d == day) {
                Some(entry) if seen => entry.2 += n,
                Some(entry) => entry.1 += n,
                None => days.push(if seen { (day, 0, n) } else { (day, n, 0) }),
            }
        }
    };
    count("SELECT (read_at + ?1) / 86400, COUNT(*) FROM files WHERE read_at >= ?2 AND state IN (1, 3) GROUP BY 1", false);
    count("SELECT (at + ?1) / 86400, COUNT(*) FROM seen WHERE at >= ?2 AND ok = 1 GROUP BY 1", true);
    days.sort_unstable();
    days
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

fn date(day: i64) -> (i64, usize, i64) {
    let (year, month, d) = common::civil(day);
    (year, month as usize, d)
}

fn short(day: i64) -> String {
    let (_, month, d) = date(day);
    format!("{} {d}", MONTHS[month - 1])
}

/// "Nov 12 to 18", "Oct 20 to Nov 18".
fn between(first: i64, last: i64) -> String {
    let ((_, a, _), (_, b, d)) = (date(first), date(last));
    if a == b { format!("{} to {d}", short(first)) } else { format!("{} to {}", short(first), short(last)) }
}

fn hour_label(hour: usize, twelve: bool) -> String {
    match (hour, twelve) {
        (0, true) => "12 AM".into(),
        (12, true) => "Noon".into(),
        (h, true) if h < 12 => format!("{h} AM"),
        (h, true) => format!("{} PM", h - 12),
        (h, false) => format!("{h}:00"),
    }
}

/// The card's four choices: today by the hour (`hours`: read and seen in each hour since midnight), then the past week
/// and month by the day and the past year by the month. Empty when nothing was read in the past year.
pub fn spans(days: &Days, hours: &[(u64, u64); 24], today: i64, twelve: bool) -> Vec<Span> {
    let mut days = days.clone();
    // Today's hours are newer than the days (asked more often).
    days.retain(|(day, _, _)| *day < today);
    let today_read = hours.iter().map(|h| h.0).sum::<u64>();
    let today_seen = hours.iter().map(|h| h.1).sum::<u64>();
    days.push((today, today_read, today_seen));
    let Some(first) = days.iter().find(|(_, read, seen)| read + seen > 0).map(|(day, _, _)| *day) else {
        return Vec::new();
    };
    let on = |day: i64| days.iter().find(|(d, _, _)| *d == day).map(|&(_, r, s)| (r, s)).unwrap_or((0, 0));
    let hour_bars: Vec<(u64, u64, String, String)> = (0..24)
        .map(|h| {
            let label = if h % 6 == 0 { hour_label(h, twelve) } else { String::new() };
            (hours[h].0, hours[h].1, label, format!("{} to {}", hour_label(h, twelve), hour_label((h + 1) % 24, twelve)))
        })
        .collect();
    let by_day = |count: i64| -> Vec<(u64, u64, String, String)> {
        (today - count + 1..=today)
            .map(|day| {
                let (r, s) = on(day);
                let at = day - (today - count + 1);
                let label = if count <= 7 { WEEKDAYS[weekday(day)].to_string() } else if at % 7 == 0 { short(day) } else { String::new() };
                (r, s, label, if day == today { "Today".into() } else { format!("{}, {}", WEEKDAYS[weekday(day)], short(day)) })
            })
            .collect()
    };
    let (year, month, _) = date(today);
    let months: Vec<(i64, usize)> = (0..12).rev().map(|back| {
        let m = (year * 12 + month as i64 - 1) - back;
        (m.div_euclid(12), m.rem_euclid(12) as usize + 1)
    }).collect();
    let month_bars: Vec<(u64, u64, String, String)> = months.iter().map(|&(y, m)| {
        let start = common::days_from_civil(y, m as i64, 1);
        let end = if m == 12 { common::days_from_civil(y + 1, 1, 1) } else { common::days_from_civil(y, m as i64 + 1, 1) };
        let (r, s) = days.iter().filter(|(d, _, _)| (start..end).contains(d)).fold((0, 0), |a, &(_, r, s)| (a.0 + r, a.1 + s));
        (r, s, MONTHS[m - 1].to_string(), format!("{} {y}", MONTHS[m - 1]))
    }).collect();
    // The average is over the time since the first reading, not before Excavate was there.
    let week_days = (today - first + 1).clamp(1, 7);
    let month_days = (today - first + 1).clamp(1, 30);
    let (first_year, first_month, _) = date(first);
    let year_months = ((year * 12 + month as i64) - (first_year * 12 + first_month as i64) + 1).clamp(1, 12);
    let (ly, lm) = months[0];
    vec![
        span("Today", "Today so far", 0, format!("{}, {}", WEEKDAYS[weekday(today)], short(today)), "hour", hour_bars),
        span("Past week", "Average per day", week_days, between(today - 6, today), "day", by_day(7)),
        span("Past month", "Average per day", month_days, between(today - 29, today), "day", by_day(30)),
        span("Past year", "Average per month", year_months, format!("{} {ly} to {} {year}", MONTHS[lm - 1], MONTHS[month - 1]), "month", month_bars),
    ]
}

/// A choice from its bars; `over` is how many bars the average is over (0 for none: the big number is the total).
fn span(choice: &'static str, kind: &'static str, over: i64, when: String, unit: &'static str, bars: Vec<(u64, u64, String, String)>) -> Span {
    let total = bars.iter().map(|b| b.0).sum::<u64>();
    let seen = bars.iter().map(|b| b.1).sum::<u64>();
    let busiest = bars.iter().map(|b| b.0).max().unwrap_or(0);
    let scale = busiest.max(1) as f32;
    let average = if over > 0 { total as f64 / over as f64 } else { -1.0 };
    Span {
        choice,
        kind,
        big: group(if over > 0 { average.round() as u64 } else { total }),
        when,
        total: group(total),
        busiest: if busiest > 0 { group(busiest) } else { "None yet".into() },
        unit,
        seen: group(seen),
        average: if over > 0 && total > 0 { (average as f32 / scale).min(1.0) } else { -1.0 },
        bars: bars.into_iter().map(|(read, seen, label, name)| Bar { share: read as f32 / scale, label, name, read: group(read), seen: group(seen) }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_average_since_the_first_reading() {
        // Wednesday, Nov 18 2026, after a week of reading.
        let today = common::days_from_civil(2026, 11, 18);
        let days: Days = vec![(today - 5, 261, 332), (today - 4, 21_226, 31_413), (today - 3, 106_051, 37_826), (today - 2, 6_740, 70_138), (today - 1, 46, 2)];
        let spans = spans(&days, &[(0, 0); 24], today, true);
        assert_eq!(spans.len(), 4);
        let week = &spans[1];
        assert_eq!((week.total.as_str(), week.big.as_str(), week.when.as_str()), ("134,324", "22,387", "Nov 12 to 18"));
        assert_eq!((week.busiest.as_str(), week.bars.len(), week.bars[3].label.as_str()), ("106,051", 7, "Sun"));
        assert_eq!((week.bars[3].share, week.bars[6].name.as_str()), (1.0, "Today"));
        assert_eq!(spans[0].big, "0");
        assert_eq!(spans[0].bars[12].label, "Noon");
        assert_eq!((spans[2].bars.len(), spans[2].when.as_str()), (30, "Oct 20 to Nov 18"));
        let year = &spans[3];
        assert_eq!((year.bars.len(), year.bars[11].read.as_str(), year.big.as_str(), year.when.as_str()), (12, "134,324", "134,324", "Dec 2025 to Nov 2026"));
        assert!(super::spans(&Vec::new(), &[(0, 0); 24], today, true).is_empty());
    }
}
