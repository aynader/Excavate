//! Summaries that say how sure a figure is: medians with a bootstrap interval, rates with a Wilson interval.

use serde_json::{Value, json};

/// The value below which `p` (0 to 1) of the values fall, by linear interpolation.
pub fn quantile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = p * (sorted.len() - 1) as f64;
    let (low, high) = (at.floor() as usize, at.ceil() as usize);
    sorted[low] + (sorted[high] - sorted[low]) * (at - low as f64)
}

pub fn median(values: &[f64]) -> f64 {
    quantile(values, 0.5)
}

/// A small, seeded generator (SplitMix64): the same seed picks the same samples on every machine.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// `count` of `items`, without repeats, in a stable order for the seed.
    pub fn sample<T: Clone>(&mut self, items: &[T], count: usize) -> Vec<T> {
        let mut picked: Vec<T> = items.to_vec();
        for i in 0..picked.len() {
            let j = i + self.below(picked.len() - i);
            picked.swap(i, j);
        }
        picked.truncate(count);
        picked
    }
}

/// The median's 95% interval, from 2,000 resamples.
pub fn median_interval(values: &[f64]) -> (f64, f64) {
    if values.len() < 2 {
        let m = median(values);
        return (m, m);
    }
    let mut rng = Rng::new(0x5EED);
    let medians: Vec<f64> = (0..2000).map(|_| {
        let resample: Vec<f64> = (0..values.len()).map(|_| values[rng.below(values.len())]).collect();
        median(&resample)
    }).collect();
    (quantile(&medians, 0.025), quantile(&medians, 0.975))
}

/// Times (or any measure): how many, the median and its interval, the 95th percentile, the extremes.
pub fn summary(values: &[f64]) -> Value {
    let (low, high) = median_interval(values);
    json!({
        "n": values.len(),
        "median": round(median(values)),
        "median_95": [round(low), round(high)],
        "p95": round(quantile(values, 0.95)),
        "min": round(quantile(values, 0.0)),
        "max": round(quantile(values, 1.0)),
    })
}

/// A share of successes, with its 95% Wilson interval.
pub fn rate(successes: usize, n: usize) -> Value {
    if n == 0 {
        return json!({ "n": 0 });
    }
    let (p, n_f, z) = (successes as f64 / n as f64, n as f64, 1.96f64);
    let centre = (p + z * z / (2.0 * n_f)) / (1.0 + z * z / n_f);
    let half = z * (p * (1.0 - p) / n_f + z * z / (4.0 * n_f * n_f)).sqrt() / (1.0 + z * z / n_f);
    json!({ "n": n, "share": round(p), "share_95": [round(centre - half), round(centre + half)] })
}

pub fn round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let scale = if value.abs() >= 100.0 { 10.0 } else if value.abs() >= 1.0 { 1000.0 } else { 100_000.0 };
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_interpolate() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(quantile(&[0.0, 10.0], 0.95), 9.5);
    }

    #[test]
    fn wilson_stays_inside() {
        let all = rate(10, 10);
        let high = all["share_95"][1].as_f64().unwrap();
        assert!(high <= 1.0 && all["share_95"][0].as_f64().unwrap() > 0.6);
    }

    #[test]
    fn samples_are_stable() {
        let items: Vec<u32> = (0..100).collect();
        assert_eq!(Rng::new(7).sample(&items, 5), Rng::new(7).sample(&items, 5));
        assert_ne!(Rng::new(7).sample(&items, 5), Rng::new(8).sample(&items, 5));
    }
}
