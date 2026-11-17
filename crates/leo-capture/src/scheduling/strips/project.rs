//! Map AOI boundaries into (acquisition time, cross-track offset) coordinates
//! for each actual pass and each retained along-track look. No global heading.
use super::*;
use geo::{LineString, Polygon};
use leo_sim::geometry::{dot, Vector};

pub(super) struct Track {
    start: f64,
    step: f64,
    points: Vec<(Vector, Vector)>,
}
impl Track {
    pub fn new(elements: &Elements, epoch: f64, horizon: f64, step: f64) -> Self {
        let start = -600.0;
        let count = ((horizon + 1200.0) / step).ceil() as usize;
        Self {
            start,
            step,
            points: (0..=count)
                .map(|i| super::super::sample(elements, epoch, start + i as f64 * step))
                .collect(),
        }
    }
    pub fn at(&self, t: f64) -> (Vector, Vector) {
        let position = ((t - self.start) / self.step).clamp(0.0, (self.points.len() - 1) as f64);
        let i = (position.floor() as usize).min(self.points.len() - 2);
        let u = position - i as f64;
        let (a, na) = self.points[i];
        let (b, nb) = self.points[i + 1];
        let q = geometry::mix(a, b, u);
        (q, geometry::normal_between(na, nb, q, u))
    }
    fn coordinates(&self, v: Vector, t: f64) -> (f64, f64) {
        let (q, n) = self.at(t);
        let tangent = geometry::cross(n, q);
        let x = dot(v, tangent);
        let y = dot(v, n);
        let sine = x.hypot(y);
        let distance = sine.atan2(dot(v, q)) * R;
        let factor = if sine < 1e-12 { R } else { distance / sine };
        (x * factor, y * factor)
    }
    fn project(&self, v: Vector, mut lo: f64, mut hi: f64, along: f64) -> Option<Coord<f64>> {
        if self.coordinates(v, lo).0 < along || self.coordinates(v, hi).0 > along {
            return None;
        }
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if self.coordinates(v, mid).0 > along {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let time = (lo + hi) / 2.0;
        let cross = self.coordinates(v, time).1;
        cross.is_finite().then_some(Coord { x: time, y: cross })
    }
}

pub(super) fn targets(
    config: &Config,
    jobs: &[Job],
    atlas: &Atlas,
    masks: &SolarMasks,
    track: &Track,
    stats: &mut Statistics,
) -> Result<(Vec<Target>, Vec<Vec<(f64, f64)>>)> {
    let options = config.strips.as_ref().unwrap();
    let step = options.access_step_s;
    let count = (config.options.duration_s / step).ceil() as usize;
    let mut windows: Vec<Vec<(f64, f64)>> = vec![Vec::new(); jobs.len()];
    // Access discovery is conservative; exact polygon overlays decide usability.
    for i in 0..count {
        let start = i as f64 * step;
        let end = ((i + 1) as f64 * step).min(config.options.duration_s);
        let (a, _) = track.at(start);
        let (b, _) = track.at(end);
        let mut visible = BTreeSet::new();
        for p in atlas.candidates(a, b, config.options.reach_km / (2.0 * R)) {
            for piece in &atlas.patches[p].pieces {
                let bits = masks.due_bits.as_ref().unwrap_or(&masks.bits);
                let first = (start / masks.step_s).floor() as usize;
                let last = ((end - 1e-9) / masks.step_s).floor() as usize;
                if (first..=last.min(bits.len() - 1)).any(|m| bits[m][piece.job]) {
                    visible.insert(piece.job);
                }
            }
        }
        for job in visible {
            if let Some(last) = windows[job]
                .last_mut()
                .filter(|last| start - last.1 <= step + 1e-9)
            {
                last.1 = end;
            } else {
                windows[job].push((start, end));
            }
        }
    }
    let mut pieces = vec![Vec::new(); jobs.len()];
    for (p, patch) in atlas.patches.iter().enumerate() {
        for (i, piece) in patch.pieces.iter().enumerate() {
            pieces[piece.job].push((p, i));
        }
    }
    let mut out = Vec::new();
    for (job, passes) in windows.iter().enumerate() {
        for &(start, end) in passes {
            stats.access_windows += 1;
            for &along in &config.options.along_offsets_km {
                let mut polygons = Vec::new();
                for &(p, i) in &pieces[job] {
                    let patch = &atlas.patches[p];
                    polygons.extend(project_shape(
                        &patch.pieces[i].shape,
                        &patch.chart,
                        track,
                        start,
                        end,
                        along,
                    ));
                }
                // Chart pieces have disjoint interiors. Preserve them, including
                // holes, in one multipolygon without unioning across chart seams.
                let shape = MultiPolygon::new(polygons);
                if let Some(bounds) = shape.bounding_rect() {
                    let limit = (config.options.reach_km - config.options.width_km) / 2.0;
                    if bounds.min().y > limit || bounds.max().y < -limit {
                        continue;
                    }
                    out.push(Target {
                        job,
                        along,
                        shape,
                        bounds,
                    });
                }
            }
        }
        if job % 256 == 0 {
            resources::check_memory()?;
        }
    }
    stats.projected_targets = out.len();
    Ok((out, windows))
}

fn project_shape(
    shape: &Shape,
    chart: &geometry::Chart,
    track: &Track,
    start: f64,
    end: f64,
    along: f64,
) -> Vec<Polygon<f64>> {
    let ring = |r: &LineString<f64>| -> Option<LineString<f64>> {
        r.0.iter()
            .map(|&v| track.project(chart.inverse(v), start - 120.0, end + 120.0, along))
            .collect::<Option<Vec<_>>>()
            .map(LineString::new)
    };
    shape
        .0
        .iter()
        .filter_map(|p| {
            Some(Polygon::new(
                ring(p.exterior())?,
                p.interiors().iter().map(ring).collect::<Option<Vec<_>>>()?,
            ))
        })
        .collect()
}

/// Project the precomputed union for actual access intervals, then reuse the
/// baseline full-length layouts. Group windows are UTC, not a synthetic AOI's
/// centroid solar gate. Exact crop evaluation still uses every original job.
pub(super) fn group_proposals(
    config: &Config,
    groups: &[crate::job_grouper::Group],
    atlas: &Atlas,
    passes: &[Vec<(f64, f64)>],
    track: &Track,
) -> Vec<Proposal> {
    let mut out = Vec::new();
    for group in groups {
        let mut intervals: Vec<_> = group
            .members
            .iter()
            .flat_map(|m| passes[m.job].iter().copied())
            .collect();
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (a, b) in intervals {
            if let Some(last) = merged.last_mut().filter(|last| a <= last.1) {
                last.1 = last.1.max(b);
            } else {
                merged.push((a, b));
            }
        }
        let seeds: Vec<_> = group.members.iter().map(|m| m.job).collect();
        for (a, b) in merged {
            for window in &group.windows {
                let (start, end) = (a.max(window.start_s), b.min(window.end_s));
                if end <= start {
                    continue;
                }
                for &along in &config.options.along_offsets_km {
                    let polygons = group
                        .pieces
                        .iter()
                        .flat_map(|p| {
                            project_shape(
                                &p.shape,
                                &atlas.patches[p.patch].chart,
                                track,
                                start,
                                end,
                                along,
                            )
                        })
                        .collect();
                    let shape = MultiPolygon::new(polygons);
                    for mut p in layouts(&shape, along, &seeds, true, config) {
                        if p.start < window.start_s || p.end > window.end_s {
                            continue;
                        }
                        p.group = Some(group.id);
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}
