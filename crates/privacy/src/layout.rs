//! Values placed by where they are in a picture, not by the order OCR reads its lines: a screen puts a value beside
//! its label (a settings dialog's two columns), under it (a password manager's fields, with a "Hide" button or a
//! sidebar's words read in between), or down its column (a table of saved passwords).

use crate::context::{code_label, value_for_label};
use crate::{Kind, Secret, iban_beside, iban_label_only, label_only, password_like};
use std::ops::Range;

/// The secrets beside, under or down the column of a label in a picture's lines (each its byte range in `text` and
/// its box, [left, top, right, bottom]).
pub fn values_by_layout(lines: &[(Range<usize>, [f32; 4])], text: &str) -> Vec<Secret> {
    let read: Vec<(&str, Range<usize>, [f32; 4])> = lines.iter().filter_map(|(range, bounds)| {
        let line = text.get(range.clone())?;
        let trimmed = line.trim();
        let start = range.start + (line.len() - line.trim_start().len());
        (!trimmed.is_empty()).then_some((trimmed, start..start + trimmed.len(), *bounds))
    }).collect();
    let is_label = |line: &str| label_only(line) || code_label(line) || iban_label_only(line);
    let mut found = Vec::new();
    for (index, (label, _, b)) in read.iter().enumerate() {
        let password = label_only(label);
        let iban = iban_label_only(label);
        if !password && !iban && !code_label(label) {
            continue;
        }
        let h = (b[3] - b[1]).max(1.0);
        // The kind of a value, and where it is in its line (an IBAN may have words beside it).
        let kind_of = |value: &str| -> Option<(Kind, Range<usize>)> {
            if iban {
                return iban_beside(label, value).map(|range| (Kind::Iban, range));
            }
            if let Some(kind) = value_for_label(label, value) {
                return Some((kind, 0..value.len()));
            }
            (password && (password_like(value) || crate::digit_groups(value))).then_some((Kind::Password, 0..value.len()))
        };
        // The nearest line beside or under it is its value, if it looks like one (not the next value-like line past
        // it: a poster's words under a "Pin").
        let mut beside: Option<(f32, usize)> = None;
        let mut column: Vec<usize> = Vec::new();
        let same_row = |a: &[f32; 4], c: &[f32; 4]| a[3].min(c[3]) - a[1].max(c[1]) >= 0.5 * (a[3] - a[1]).min(c[3] - c[1]);
        // A table's header: other headers on its row.
        let header = read.iter().enumerate().any(|(other, (_, _, v))| other != index && same_row(b, v));
        for (other, (value, _, v)) in read.iter().enumerate() {
            if other == index || is_label(value) {
                continue;
            }
            let vh = (v[3] - v[1]).max(1.0);
            // Beside it: on its row, allowing for a photo's tilt (up to about 7 degrees) over the gap between them.
            let gap = v[0] - b[2];
            let drift = ((v[1] + v[3]) - (b[1] + b[3])).abs() / 2.0;
            let right = gap >= -0.5 * h && gap <= 25.0 * h && drift <= 0.5 * h.min(vh) + 0.13 * gap.max(0.0);
            let aligned = (v[0] - b[0]).abs() <= 3.0 * h || (v[0] < b[2] && v[2] > b[0]);
            let below = v[1] >= b[1] + 0.5 * h && v[1] - b[3] <= 2.5 * h && aligned;
            let distance = if right { v[0] - b[2] } else if below { v[1] - b[3] } else { f32::MAX };
            if distance < f32::MAX && beside.is_none_or(|(best, _)| distance < best) {
                beside = Some((distance, other));
            }
            // Down a table's column, under its header, in a row of the table (other cells beside it).
            let in_row = read.iter().enumerate().any(|(cell, (_, _, c))| cell != other && same_row(v, c));
            if password && header && in_row && v[1] >= b[3] && v[1] - b[3] <= 14.0 * h && (v[0] - b[0]).abs() <= 2.0 * h {
                column.push(other);
            }
        }
        for other in beside.map(|(_, other)| other).into_iter().chain(column) {
            if let Some((kind, within)) = kind_of(read[other].0) {
                let start = read[other].1.start;
                found.push(Secret { kind, range: start + within.start..start + within.end });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as (text, box): joined as the indexer joins them, in reading order.
    fn place(lines: &[(&str, [f32; 4])]) -> (String, Vec<(Range<usize>, [f32; 4])>) {
        let mut text = String::new();
        let mut placed = Vec::new();
        for (line, bounds) in lines {
            let start = text.len();
            text.push_str(line);
            placed.push((start..text.len(), *bounds));
            text.push('\n');
        }
        (text, placed)
    }

    fn values(lines: &[(&str, [f32; 4])]) -> Vec<String> {
        let (text, placed) = place(lines);
        crate::merged(values_by_layout(&placed, &text)).into_iter().map(|secret| text[secret.range].to_string()).collect()
    }

    #[test]
    fn beside_its_label() {
        // A settings dialog: the value a little higher than its label, so read before it.
        let lines = [("Cobalt2008%", [300.0, 98.0, 420.0, 116.0]), ("Security key", [30.0, 100.0, 140.0, 118.0]),
                     ("[x] Show characters", [300.0, 140.0, 460.0, 158.0])];
        assert_eq!(values(&lines), ["Cobalt2008%"]);
    }

    #[test]
    fn under_its_label_with_words_between() {
        // A password manager: "Hide" beside the label's value, a sidebar's word read between.
        let lines = [("Password", [300.0, 200.0, 380.0, 214.0]), ("Forum", [24.0, 205.0, 80.0, 222.0]), ("Hide", [720.0, 222.0, 760.0, 240.0]),
                     ("r49pn8j5du", [300.0, 222.0, 420.0, 240.0]), ("Verification code", [300.0, 280.0, 420.0, 294.0]),
                     ("Shop", [24.0, 290.0, 70.0, 306.0]), ("544 423", [300.0, 302.0, 380.0, 324.0])];
        assert_eq!(values(&lines), ["r49pn8j5du", "544 423"]);
    }

    #[test]
    fn beside_it_in_a_tilted_photo() {
        // Six degrees over 160 pixels: the value's row 17 pixels lower than its label's.
        let lines = [("Security key", [30.0, 100.0, 140.0, 118.0]), ("Cobalt2008%", [300.0, 117.0, 420.0, 135.0])];
        assert_eq!(values(&lines), ["Cobalt2008%"]);
    }

    #[test]
    fn down_a_column() {
        let lines = [("Site", [30.0, 100.0, 70.0, 116.0]), ("Username", [360.0, 100.0, 450.0, 116.0]), ("Password", [700.0, 100.0, 790.0, 116.0]),
                     ("mail.example.com", [30.0, 140.0, 200.0, 156.0]), ("sam1@example.com", [360.0, 140.0, 520.0, 156.0]),
                     ("mail.example.com", [30.0, 180.0, 200.0, 196.0]), ("jo55@example.com", [360.0, 180.0, 520.0, 196.0]),
                     ("nqyzH#x&T%KF", [700.0, 180.0, 830.0, 196.0])];
        assert_eq!(values(&lines), ["nqyzH#x&T%KF"]);
    }

    #[test]
    fn an_iban_beside_its_label() {
        // A banking app's two columns, the IBAN made up (its check digits fail), and one under its label with words read
        // between (OCR reads the sidebar's line first).
        let lines = [("Account holder", [30.0, 100.0, 160.0, 116.0]), ("Alex Morgan", [300.0, 100.0, 420.0, 116.0]),
                     ("IBAN", [30.0, 140.0, 80.0, 156.0]), ("DE12 3456 7890 1234 5678 90", [300.0, 140.0, 560.0, 156.0])];
        assert_eq!(values(&lines), ["DE12 3456 7890 1234 5678 90"]);
        let lines = [("Account number", [300.0, 200.0, 440.0, 214.0]), ("Payments", [24.0, 205.0, 110.0, 222.0]),
                     ("GB82 WEST 1234 5698 7654 33  Copy", [300.0, 222.0, 640.0, 240.0])];
        assert_eq!(values(&lines), ["GB82 WEST 1234 5698 7654 33"]);
        // Not an IBAN's shape: nothing.
        let lines = [("Account", [30.0, 100.0, 110.0, 116.0]), ("Alex Morgan", [300.0, 100.0, 420.0, 116.0])];
        assert!(values(&lines).is_empty());
    }

    #[test]
    fn a_posters_words_under_pin() {
        let lines = [("Pin", [100.0, 100.0, 140.0, 120.0]), ("KAR", [100.0, 126.0, 150.0, 146.0]), ("L.iTTLE", [100.0, 152.0, 190.0, 172.0])];
        assert!(values(&lines).is_empty(), "{:?}", values(&lines));
    }

    #[test]
    fn a_sign_in_screen_has_nothing() {
        let lines = [("Email", [40.0, 100.0, 90.0, 114.0]), ("name@example.com", [52.0, 124.0, 220.0, 140.0]),
                     ("Password", [40.0, 170.0, 120.0, 184.0]), ("00000000", [52.0, 194.0, 140.0, 210.0]),
                     ("Forgot password?", [40.0, 240.0, 200.0, 256.0]), ("Remember me", [40.0, 270.0, 160.0, 286.0]),
                     ("Sign in", [40.0, 300.0, 110.0, 316.0])];
        assert!(values(&lines).is_empty(), "{:?}", values(&lines));
    }
}
