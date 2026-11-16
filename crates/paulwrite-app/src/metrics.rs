//! Mesures de latence : échantillons et résumé (moyenne, percentiles).

use std::fmt;
use std::time::Duration;

/// Série d'échantillons de durée.
#[derive(Clone, Debug, Default)]
pub struct Samples {
    micros: Vec<f64>,
}

/// Résumé statistique, en millisecondes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Samples {
    pub fn push(&mut self, d: Duration) {
        self.micros.push(d.as_secs_f64() * 1e6);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.micros.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.micros.is_empty()
    }

    /// Résumé, ou `None` s'il n'y a aucun échantillon.
    #[must_use]
    pub fn summary(&self) -> Option<Summary> {
        if self.micros.is_empty() {
            return None;
        }
        let mut s = self.micros.clone();
        s.sort_by(f64::total_cmp);
        let q = |p: f64| s[((s.len() as f64 - 1.0) * p).round() as usize] / 1000.0;
        Some(Summary {
            count: s.len(),
            mean: s.iter().sum::<f64>() / s.len() as f64 / 1000.0,
            p50: q(0.5),
            p95: q(0.95),
            p99: q(0.99),
            max: q(1.0),
        })
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={} moy {:.2} ms | p50 {:.2} | p95 {:.2} | p99 {:.2} | max {:.2}",
            self.count, self.mean, self.p50, self.p95, self.p99, self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut s = Samples::default();
        assert!(s.summary().is_none());
        for ms in 1..=100 {
            s.push(Duration::from_millis(ms));
        }
        let r = s.summary().unwrap();
        assert_eq!(r.count, 100);
        assert!((r.mean - 50.5).abs() < 1e-9);
        assert!((r.p50 - 51.0).abs() < 1.5);
        assert!((r.p99 - 99.0).abs() < 1.5);
        assert!((r.max - 100.0).abs() < 1e-9);
        assert!(format!("{r}").contains("p99"));
    }
}
