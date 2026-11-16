//! Constellation-derived service design, not a certificate of joint schedulability.
//! Width bounds assume parallel, fixed-direction strips, one strip per pass.
//! Shared capacity is a configurable reservation policy, not a conflict-clique proof.
use crate::{
    geometry::{self, Region},
    workload::{hash, Fulfilment, Illumination, Job, MosaicTiming},
};
use leo_sim::{
    geometry::{dot, ground_position, EARTH_RADIUS_KM as R},
    worldview::Fleet,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub imaging_width_km: f64,
    pub access_width_km: f64,
    pub strip_overlap_fraction: f64,
    /// Reference from the fleet's sampled access study; not an all-time bound.
    pub nominal_access_gap_hours: f64,
    /// Reservation budget shared by both products. Close-in-time jobs may use
    /// other daylight hours; the budget does not count those as extra capacity.
    pub usable_window_hours_per_day: f64,
    pub similar_solar_time_fraction: f64,
    pub stagger_delivery_phases: bool,
    /// None derives lanes from access width / strip spacing.
    pub reserved_lanes: Option<u32>,
    pub epoch_unix_s: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            imaging_width_km: leo_sim::worldview::IMAGING_SWATH_KM,
            access_width_km: leo_sim::worldview::ACCESS_WIDTH_KM,
            strip_overlap_fraction: 0.1,
            nominal_access_gap_hours: 2.4,
            usable_window_hours_per_day: 8.0,
            similar_solar_time_fraction: 0.5,
            stagger_delivery_phases: true,
            reserved_lanes: None,
            epoch_unix_s: 1794528000.0,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if !self.imaging_width_km.is_finite()
            || self.imaging_width_km <= 0.0
            || !self.access_width_km.is_finite()
            || self.access_width_km < self.imaging_width_km
            || !self.strip_overlap_fraction.is_finite()
            || !(0.0..0.9).contains(&self.strip_overlap_fraction)
            || !self.nominal_access_gap_hours.is_finite()
            || self.nominal_access_gap_hours <= 0.0
            || !self.usable_window_hours_per_day.is_finite()
            || !(0.0..=24.0).contains(&self.usable_window_hours_per_day)
            || self.usable_window_hours_per_day < self.nominal_access_gap_hours
            || !self.similar_solar_time_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.similar_solar_time_fraction)
            || self.reserved_lanes == Some(0)
            || !self.epoch_unix_s.is_finite()
        {
            return Err("Invalid constellation admission policy".into());
        }
        Ok(())
    }
    pub fn strip_spacing_km(&self) -> f64 {
        self.imaging_width_km * (1.0 - self.strip_overlap_fraction)
    }
    pub fn lanes(&self) -> u32 {
        self.reserved_lanes
            .unwrap_or((self.access_width_km / self.strip_spacing_km()).ceil() as u32)
    }
    pub fn daily_pass_budget(&self) -> u32 {
        (self.usable_window_hours_per_day / self.nominal_access_gap_hours).floor() as u32
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Basis {
    pub model: String,
    pub geometry_hash: u64,
    pub selected_ground_track_bearing_deg: f64,
    pub perpendicular_extent_km: f64,
    pub parallel_strips_lower_bound: u32,
    pub planned_strips_with_overlap: u32,
    pub nominal_passes_per_day: u32,
    pub reserved_usable_window_hours_per_day: f64,
    pub reserved_lanes: u32,
    pub cadence_floor_hours: f64,
    pub collection_floor_hours: f64,
}
impl Basis {
    pub fn validate(&self) -> Result<(), String> {
        if self.model != "worldview60_parallel_strip_reservation_v1"
            || !self.selected_ground_track_bearing_deg.is_finite()
            || !self.perpendicular_extent_km.is_finite()
            || self.perpendicular_extent_km < 0.0
            || self.parallel_strips_lower_bound == 0
            || self.planned_strips_with_overlap < self.parallel_strips_lower_bound
            || self.nominal_passes_per_day == 0
            || !self.reserved_usable_window_hours_per_day.is_finite()
            || !(0.0..=24.0).contains(&self.reserved_usable_window_hours_per_day)
            || self.reserved_usable_window_hours_per_day == 0.0
            || self.reserved_lanes == 0
            || !self.cadence_floor_hours.is_finite()
            || self.cadence_floor_hours <= 0.0
            || !self.collection_floor_hours.is_finite()
            || self.collection_floor_hours <= 0.0
            || self.collection_floor_hours > self.cadence_floor_hours
        {
            return Err("Invalid saved admission basis".into());
        }
        Ok(())
    }
}

/// Greedy cover of projected component intervals. Empty gaps between islands
/// do not require strips. Holes cannot remove a connected component's projection.
fn cover(intervals: &[(f64, f64)], width: f64, spacing: f64) -> u32 {
    let mut sorted = intervals.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut end = f64::NEG_INFINITY;
    let mut count = 0;
    for (lo, hi) in sorted {
        if hi <= end + 1e-8 {
            continue;
        }
        if lo > end {
            count += 1;
            end = lo + width;
        }
        if hi > end + 1e-8 {
            let more = ((hi - end - 1e-8) / spacing).ceil() as u32;
            count += more;
            end += f64::from(more) * spacing;
        }
    }
    count.max(1)
}

/// Ascending/descending Earth-fixed headings sampled from the actual design
/// orbit, including its J2 propagation and Earth rotation. Near the turning
/// latitude use the closest subsatellite latitude (off-nadir access still matters).
fn bearings(latitude: f64, epoch: f64) -> [f64; 2] {
    let e = Fleet::WorldView60.shell().elements(0, 0);
    let period = std::f64::consts::TAU / (e.mu / e.semi_major_km.powi(3)).sqrt();
    let limit = e.inclination_rad.sin().asin().to_degrees().abs() - 0.001;
    let target = latitude.clamp(-limit, limit);
    [true, false].map(|ascending| {
        let (mut lo, mut hi) = if ascending {
            (-period / 4.0, period / 4.0)
        } else {
            (period / 4.0, 3.0 * period / 4.0)
        };
        for _ in 0..48 {
            let t = (lo + hi) / 2.0;
            if (e.sample(t).lat_deg < target) == ascending {
                lo = t;
            } else {
                hi = t;
            }
        }
        let (q, n) = crate::scheduling::sample(&e, epoch, (lo + hi) / 2.0);
        let along = geometry::cross(n, q);
        let lon = (-q[2]).atan2(q[0]).to_degrees();
        let east = ground_position(0.0, lon + 90.0, 1.0, 0.0);
        let north = geometry::cross(q, east);
        dot(along, east).atan2(dot(along, north))
    })
}

pub fn assess(job: &Job, region: &Region, policy: &Policy) -> Result<Basis, String> {
    policy.validate()?;
    if job.aoi.key() != region.job_key
        || hash(&serde_json::to_vec(&region.polygons).map_err(|e| e.to_string())?)
            != job.aoi.geometry_hash
    {
        return Err("Admission geometry does not match job".into());
    }
    let origin = ground_position(job.aoi.latitude, job.aoi.longitude, 1.0, 0.0);
    let east = ground_position(0.0, job.aoi.longitude + 90.0, 1.0, 0.0);
    let north = geometry::cross(origin, east);
    let mut parts = Vec::new();
    for polygon in &region.polygons {
        let mut points = Vec::new();
        for ring in polygon {
            // Sample lon/lat edges, preserving the same source geometry as the atlas.
            for pair in ring.windows(2) {
                let steps = (((pair[1][0] - pair[0][0])
                    .abs()
                    .max((pair[1][1] - pair[0][1]).abs()))
                    / 0.05)
                    .ceil()
                    .max(1.0) as usize;
                for k in 0..=steps {
                    let u = k as f64 / steps as f64;
                    let lon = pair[0][0] + u * (pair[1][0] - pair[0][0]);
                    let lat = pair[0][1] + u * (pair[1][1] - pair[0][1]);
                    let p = ground_position(lat, lon, 1.0, 0.0);
                    let angle = dot(origin, p).clamp(-1.0, 1.0).acos();
                    if angle > 1.5 {
                        return Err(format!(
                            "{} is too large for the local strip admission model",
                            region.job_key
                        ));
                    }
                    let scale = if angle < 1e-10 {
                        R
                    } else {
                        R * angle / angle.sin()
                    };
                    points.push((scale * dot(p, east), scale * dot(p, north)));
                }
            }
        }
        if !points.is_empty() {
            parts.push(points);
        }
    }
    if parts.is_empty() {
        return Err("Empty admission geometry".into());
    }
    let mut choices = Vec::new();
    for bearing in bearings(job.aoi.latitude, policy.epoch_unix_s) {
        let intervals: Vec<_> = parts
            .iter()
            .map(|points| {
                points
                    .iter()
                    .map(|&(x, y)| x * bearing.cos() - y * bearing.sin())
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                        (lo.min(p), hi.max(p))
                    })
            })
            .collect();
        let lower = cover(&intervals, policy.imaging_width_km, policy.imaging_width_km);
        let strips = cover(
            &intervals,
            policy.imaging_width_km,
            policy.strip_spacing_km(),
        );
        let extent = intervals
            .iter()
            .map(|x| x.1)
            .fold(f64::NEG_INFINITY, f64::max)
            - intervals.iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
        choices.push((strips, lower, extent, bearing));
    }
    choices.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.total_cmp(&b.2)));
    let (strips, lower, extent, bearing) = choices[0];
    let passes = policy.daily_pass_budget();
    // Whole-day service contracts avoid treating a fractional day as an extra
    // daily solar window. The floor is a reservation policy, not measured mutex.
    let cadence = (f64::from(strips) * f64::from(policy.lanes()) / f64::from(passes)).ceil() * 24.0;
    let collection = f64::from(strips.div_ceil(passes)) * 24.0;
    let basis = Basis {
        model: "worldview60_parallel_strip_reservation_v1".into(),
        geometry_hash: job.aoi.geometry_hash,
        selected_ground_track_bearing_deg: bearing.to_degrees(),
        perpendicular_extent_km: extent,
        parallel_strips_lower_bound: lower,
        planned_strips_with_overlap: strips,
        nominal_passes_per_day: passes,
        reserved_usable_window_hours_per_day: policy.usable_window_hours_per_day,
        reserved_lanes: policy.lanes(),
        cadence_floor_hours: cadence,
        collection_floor_hours: collection,
    };
    basis.validate()?;
    Ok(basis)
}

/// Preserve AOIs, pipelines, output densities, priorities and fulfilment. Adjust
/// cadence/history and make the collection contract explicit. No feasibility claim.
pub fn redesign(job: &Job, region: &Region, policy: &Policy) -> Result<Job, String> {
    let basis = assess(job, region, policy)?;
    let mut next = job.clone();
    next.acquisition.first_deadline_hours = None;
    let history = job.span_hours();
    next.cadence_hours = job.cadence_hours.max(basis.cadence_floor_hours);
    if policy.stagger_delivery_phases {
        let days = (next.cadence_hours / 24.0).floor().max(1.0) as u64;
        next.acquisition.phase_hours =
            (hash(format!("delivery:{}", next.aoi.key()).as_bytes()) % days) as f64 * 24.0;
    }
    if next.pipeline.has_window() {
        next.samples = ((history / next.cadence_hours).ceil() as u32 + 1).max(2);
    }
    next.acquisition.illumination = Illumination::Daylight;
    next.acquisition.solar_time_hours = 12.0;
    next.acquisition.solar_tolerance_hours = policy.usable_window_hours_per_day / 2.0;
    if next.acquisition.fulfilment == Fulfilment::Mosaic {
        let draw = hash(next.aoi.key().as_bytes()) as f64 / u64::MAX as f64;
        next.acquisition.mosaic_timing = if draw < policy.similar_solar_time_fraction {
            MosaicTiming::SimilarSolarTime
        } else {
            MosaicTiming::CloseInTime
        };
        next.acquisition.mosaic_span_hours = Some(
            if next.acquisition.mosaic_timing == MosaicTiming::SimilarSolarTime {
                next.cadence_hours
            } else {
                basis.collection_floor_hours
            },
        );
    } else {
        next.acquisition.mosaic_timing = MosaicTiming::CadenceWindow;
        next.acquisition.mosaic_span_hours = None;
    }
    next.acquisition.admission = Some(basis);
    next.acquisition.validate_timing(next.cadence_hours)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projected_width_counts_strips_but_not_empty_water_between_islands() {
        assert_eq!(cover(&[(0.0, 40.0)], 13.1, 13.1), 4);
        assert_eq!(cover(&[(0.0, 2.0), (100.0, 102.0)], 13.1, 13.1), 2);
        assert_eq!(cover(&[(0.0, 26.0)], 13.1, 13.1), 2);
        assert_eq!(cover(&[(0.0, 26.0)], 13.1, 11.79), 3);
    }
    #[test]
    fn reservation_policy_matches_the_forty_over_three_example() {
        let p = Policy {
            reserved_lanes: Some(40),
            ..Default::default()
        };
        p.validate().unwrap();
        assert_eq!(p.daily_pass_budget(), 3);
        assert_eq!(
            (f64::from(p.lanes()) / f64::from(p.daily_pass_budget())).ceil() * 24.0,
            336.0
        );
        assert_eq!(Policy::default().lanes(), 39);
        assert!(Policy {
            nominal_access_gap_hours: f64::NAN,
            ..p
        }
        .validate()
        .is_err());
    }
    #[test]
    fn redesign_uses_polygon_width_and_staggers_short_collection_windows() {
        use crate::workload::{AoiRef, Generator};
        let polygons = vec![vec![vec![
            [-0.2, -0.001],
            [0.2, -0.001],
            [0.2, 0.001],
            [-0.2, 0.001],
            [-0.2, -0.001],
        ]]];
        let hash = hash(&serde_json::to_vec(&polygons).unwrap());
        let mut jobs = Generator::default()
            .generate((0..30).map(|i| AoiRef {
                source: "test".into(),
                id: i.to_string(),
                area_km2: 10.0,
                longitude: 0.0,
                latitude: 0.0,
                geometry_hash: hash,
            }))
            .unwrap();
        let p = Policy {
            similar_solar_time_fraction: 0.0,
            ..Default::default()
        };
        let mut phases = std::collections::BTreeSet::new();
        for j in &mut jobs {
            j.cadence_hours = 24.0;
            j.acquisition.fulfilment = Fulfilment::Mosaic;
            let r = Region {
                job_key: j.aoi.key(),
                polygons: polygons.clone(),
            };
            let mut designed = redesign(j, &r, &p).unwrap();
            let basis = designed.acquisition.admission.as_ref().unwrap();
            assert_eq!(basis.parallel_strips_lower_bound, 4);
            assert_eq!(basis.planned_strips_with_overlap, 4);
            assert_eq!(designed.cadence_hours, 52.0 * 24.0);
            assert_eq!(designed.acquisition.mosaic_span_hours, Some(48.0));
            assert!(designed.acquisition.phase_hours < designed.cadence_hours);
            phases.insert(designed.acquisition.phase_hours as u32);
            designed.acquisition.mosaic_timing = MosaicTiming::SimilarSolarTime;
            designed.acquisition.solar_tolerance_hours = 1.0;
            assert!(designed
                .acquisition
                .validate_timing(designed.cadence_hours)
                .is_err());
        }
        assert!(phases.len() > 10);
    }
}
