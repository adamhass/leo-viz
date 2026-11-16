//! Reproducible initial deadlines for an already-running workload.
use crate::workload::{hash, Job};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub seed: u64,
    pub minimum_deadline_hours: f64,
}
impl Policy {
    /// Uniform conditional draw on [minimum, cadence), independently keyed by
    /// job identity. Resampling/truncation semantics: no clamped mass at minimum.
    /// Validate everything first so errors never leave a partially changed set.
    pub fn assign(&self, jobs: &mut [Job]) -> Result<(), String> {
        if !self.minimum_deadline_hours.is_finite() || self.minimum_deadline_hours <= 0.0 {
            return Err("Minimum initial deadline must be finite and positive".into());
        }
        for j in jobs.iter() {
            if !j.cadence_hours.is_finite() || j.cadence_hours <= self.minimum_deadline_hours {
                return Err(format!(
                    "{}: cadence must exceed the initial deadline cutoff",
                    j.aoi.key()
                ));
            }
        }
        for j in jobs {
            // SplitMix64 finalizer decorrelates similar job keys and seeds.
            let mut x = (hash(j.aoi.key().as_bytes()) ^ self.seed).wrapping_add(0x9e3779b97f4a7c15);
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
            x ^= x >> 31;
            let u = (x >> 11) as f64 / ((1_u64 << 53) as f64);
            j.acquisition.first_deadline_hours = Some(
                self.minimum_deadline_hours + u * (j.cadence_hours - self.minimum_deadline_hours),
            );
            j.acquisition.phase_hours = 0.0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{Acquisition, AoiRef, Generator};
    #[test]
    fn ongoing_deadlines_keep_cadence_and_do_not_count_prehistory() {
        let mut a = Acquisition {
            first_deadline_hours: Some(5.0),
            ..Default::default()
        };
        assert_eq!(
            a.collection_window_s(0, 312.0),
            (-307.0 * 3600.0, 5.0 * 3600.0)
        );
        for (hours, expected) in [
            (0.0, (0, true)),
            (4.999, (0, true)),
            (5.0, (1, false)),
            (24.0, (1, true)),
            (317.0, (2, false)),
        ] {
            assert_eq!(a.deadline_counts(312.0, hours * 3600.0), expected);
        }
        a.first_deadline_hours = None;
        a.phase_hours = 2.0;
        assert_eq!(a.first_deadline_s(312.0), 314.0 * 3600.0);
        assert_eq!(a.deadline_counts(312.0, 86400.0), (0, true));
        let old: Acquisition = serde_json::from_str(r#"{"phase_hours":2}"#).unwrap();
        assert_eq!(old.first_deadline_hours, None);
        assert_eq!(old.first_deadline_s(312.0), 314.0 * 3600.0);
        for invalid in [0.0, -1.0, 313.0, f64::NAN] {
            a.first_deadline_hours = Some(invalid);
            assert!(a.validate(312.0).is_err());
        }
    }
    #[test]
    fn assignments_are_bounded_reproducible_order_independent_and_spread_uniformly() {
        let mut jobs = Generator::default()
            .generate((0..10_000).map(|i| AoiRef {
                source: "test".into(),
                id: i.to_string(),
                area_km2: 1.0,
                longitude: 0.0,
                latitude: 0.0,
                geometry_hash: 1,
            }))
            .unwrap();
        for j in &mut jobs {
            j.cadence_hours = 312.0;
        }
        let original = jobs.clone();
        let policy = Policy {
            seed: 20261113,
            minimum_deadline_hours: 5.0,
        };
        policy.assign(&mut jobs).unwrap();
        let mut again = original.clone();
        again.reverse();
        policy.assign(&mut again).unwrap();
        again.reverse();
        assert_eq!(jobs, again);
        let mut bins = [0; 10];
        for (j, old) in jobs.iter().zip(&original) {
            let d = j.acquisition.first_deadline_hours.unwrap();
            assert!(d > 5.0 && d < 312.0);
            bins[((d - 5.0) / 307.0 * 10.0).floor() as usize] += 1;
            let mut restored = j.clone();
            restored.acquisition = old.acquisition.clone();
            assert_eq!(&restored, old);
        }
        assert!(bins.iter().all(|&n| (850..1150).contains(&n)), "{bins:?}");
        let mut invalid = original;
        invalid[9999].cadence_hours = 5.0;
        let before = invalid.clone();
        assert!(policy.assign(&mut invalid).is_err());
        assert_eq!(invalid, before);
    }
}
