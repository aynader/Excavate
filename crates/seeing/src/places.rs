//! Where a photo was taken, by name: the nearest place of 15,000 people or more to its GPS position, from GeoNames
//! (CC BY 4.0, https://www.geonames.org; made by tools/places/make.py), on this PC. A model can't know "Prague" from
//! the pixels; this does.

/// A place's name, its region's and its country's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub region: String,
    pub country: String,
}

impl Place {
    /// "Prague|Prague|Czechia", as the facts' file keeps it.
    pub fn joined(&self) -> String {
        format!("{}|{}|{}", self.name, self.region, self.country)
    }

    pub fn split(text: &str) -> Option<Place> {
        let mut parts = text.split('|');
        Some(Place { name: parts.next()?.to_string(), region: parts.next()?.to_string(), country: parts.next()?.to_string() })
    }

    /// Every way a search may name it, lowercase.
    pub fn names(&self) -> impl Iterator<Item = String> + '_ {
        [&self.name, &self.region, &self.country].into_iter().filter(|name| !name.is_empty()).map(|name| name.to_lowercase())
    }
}

const TABLE: &str = include_str!("../data/places.tsv");

/// Farther than this from any place in the table, a photo is said to be nowhere in particular.
const NEAR_KM: f64 = 100.0;

pub struct Places {
    /// Latitude, longitude, people, and the names ("name\tregion\tcountry").
    rows: Vec<(f32, f32, u32, &'static str)>,
}

impl Places {
    pub fn load() -> Places {
        let rows = TABLE.lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.rsplitn(4, '\t');
                let lon: f32 = fields.next()?.parse().ok()?;
                let lat: f32 = fields.next()?.parse().ok()?;
                let people: u32 = fields.next()?.parse().ok()?;
                Some((lat, lon, people, fields.next()?))
            })
            .collect();
        Places { rows }
    }

    /// The place a position is named after, if one is near enough: the nearest, unless a much bigger place is nearly as
    /// near (the middle of Cairo is nearest one of its districts, which has a place of its own in the list).
    pub fn nearest(&self, latitude: f64, longitude: f64) -> Option<Place> {
        let (lat, lon) = (latitude as f32, longitude as f32);
        let shrink = lat.to_radians().cos();
        // Distance on a plane at this latitude, in km: plenty for "which is nearest" within a hundred kilometres.
        let distance = |row_lat: f32, row_lon: f32| {
            let mut east = (row_lon - lon).abs();
            if east > 180.0 {
                east = 360.0 - east;
            }
            let (dy, dx) = (row_lat - lat, east * shrink);
            (dy * dy + dx * dx).sqrt() * 111.2
        };
        let nearest = self.rows.iter().map(|row| distance(row.0, row.1)).fold(f32::MAX, f32::min);
        if f64::from(nearest) > NEAR_KM {
            return None;
        }
        // Within twice that (and at least 10 km), the biggest.
        let reach = (nearest * 2.0).max(10.0);
        let (_, _, _, names) = self.rows.iter().filter(|row| distance(row.0, row.1) <= reach).max_by_key(|row| row.2)?;
        let mut fields = names.split('\t');
        Some(Place { name: fields.next()?.to_string(), region: fields.next().unwrap_or("").to_string(), country: fields.next().unwrap_or("").to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_from_positions() {
        let places = Places::load();
        assert!(places.rows.len() > 30_000);
        // Old Town Square, Prague; the pyramids of Giza; a beach near Sydney; the middle of the Pacific.
        assert_eq!(places.nearest(50.0875, 14.4213).unwrap().joined(), "Prague|Prague|Czechia");
        assert_eq!(places.nearest(29.9792, 31.1342).unwrap().country, "Egypt");
        assert_eq!(places.nearest(30.0444, 31.2357).unwrap().name, "Cairo");
        assert_eq!(places.nearest(-33.8908, 151.2743).unwrap().country, "Australia");
        assert!(places.nearest(-20.0, -140.0).is_none());
        let place = Place::split("Prague|Prague|Czechia").unwrap();
        assert_eq!(place.names().collect::<Vec<_>>(), ["prague", "prague", "czechia"]);
    }
}
