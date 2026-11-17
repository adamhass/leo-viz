//! Add shared collection geometry without changing any original job contract.
pub mod export;
#[cfg(test)]
mod tests;

use crate::{
    geometry::{self, Atlas, Shape},
    workload::{Illumination, Job},
};
use geo::{Area, BooleanOps};
use leo_sim::geometry::{
    dot, gmst, ground_position, local_solar_time_hours, sun_direction, Vector, EARTH_RADIUS_KM as R,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

type Result<T> = std::result::Result<T, String>;

/// Enrich a workload with optional, shared AOIs for the existing strip builder.
///
/// 1. Expand jobs into observation requests whose collection windows intersect
///    this planning horizon (optionally only deadlines due in the horizon).
///    Intersect each request with sampled local-solar-time/daylight eligibility.
/// 2. Find actual polygon overlaps using the atlas's spatial index and holes.
///    Rank links by overlap fraction, then area, with stable job/slot tie breaks.
/// 3. Greedily merge connected requests, maintaining a common usable UTC window
///    across *every* member. Different cadences are allowed; two observations of
///    the same job never enter one group. Limit membership and geographic span,
///    and require estimated mosaic strips to fit the shared access-time budget.
/// 4. Union accepted groups' polygons independently in each existing equal-area
///    chart. Preserve holes and chart boundaries, and retain job/slot membership.
///
/// `jobs` is returned unchanged by reference. `groups` are additional collection
/// targets, not synthetic reward-bearing jobs. Generate full strips from each
/// union, allow a strip to serve a subset of members, and evaluate its crops
/// against the original jobs. Keep individual-AOI candidates as the baseline.
///
/// Size admission is a heuristic: headings come from the design orbit, access
/// spacing is nominal, and solar eligibility is sampled at each job's anchor.
/// It neither reserves fleet capacity nor certifies a feasible mosaic. Actual
/// pass geometry and exact per-job collection/illumination gates remain the
/// graph builder's responsibility. The function is deterministic except timings.
pub fn enrich<'a>(
    jobs: &'a [Job],
    atlas: &Atlas,
    context: &Context,
    policy: &Policy,
) -> Result<EnrichedJobs<'a>> {
    context.validate()?;
    policy.validate()?;
    if atlas.job_areas_km2.len() != jobs.len() {
        return Err("Grouper atlas and job list must match".into());
    }
    let started = Instant::now();
    let mut report = Report {
        original_jobs: jobs.len(),
        ..Default::default()
    };
    let requests = observation_requests(jobs, context, policy, &mut report)?;
    report.window_s = started.elapsed().as_secs_f64();
    let overlap_start = Instant::now();
    let links = overlap_links(jobs, atlas, &requests, policy, &mut report);
    report.overlap_s = overlap_start.elapsed().as_secs_f64();
    let merge_start = Instant::now();
    let candidates = merge_requests(jobs, atlas, &requests, links, context, policy, &mut report);
    report.merge_s = merge_start.elapsed().as_secs_f64();
    let union_start = Instant::now();
    let out = union_groups(atlas, candidates, jobs.len());
    report.groups_added = out.len();
    report.grouped_requests = out.iter().map(|g| g.members.len()).sum();
    report.grouped_jobs = out
        .iter()
        .flat_map(|g| g.members.iter().map(|m| m.job))
        .collect::<BTreeSet<_>>()
        .len();
    report.members_per_group = Distribution::new(out.iter().map(|g| g.members.len() as f64));
    report.strips_per_group = Distribution::new(out.iter().map(|g| g.estimate.strips as f64));
    report.common_window_hours = Distribution::new(out.iter().map(|g| g.estimate.usable_hours));
    report.union_s = union_start.elapsed().as_secs_f64();
    report.elapsed_s = started.elapsed().as_secs_f64();
    Ok(EnrichedJobs {
        jobs,
        groups: out,
        report,
    })
}

fn observation_requests(
    jobs: &[Job],
    context: &Context,
    policy: &Policy,
    report: &mut Report,
) -> Result<Vec<Request>> {
    let frames = solar_frames(context, policy);
    let mut requests = Vec::new();
    for (job, j) in jobs.iter().enumerate() {
        if !j.cadence_hours.is_finite() || j.cadence_hours <= 0.0 {
            return Err("Grouper requires positive finite cadences".into());
        }
        j.acquisition.validate(j.cadence_hours)?;
        let (due, pending) = j
            .acquisition
            .deadline_counts(j.cadence_hours, context.horizon_s);
        let count = due + usize::from(!context.due_only && pending);
        if count > 1_000_000 || report.requests_considered + count > 1_000_000 {
            return Err("Too many grouping requests; shorten the horizon".into());
        }
        for slot in 0..count {
            let (start, end) = j
                .acquisition
                .collection_window_s(slot as u64, j.cadence_hours);
            let window = Window {
                start_s: start.max(0.0),
                end_s: end.min(context.horizon_s),
            };
            if window.end_s <= window.start_s {
                continue;
            }
            report.requests_considered += 1;
            let windows = usable_windows(j, window, &frames, policy.solar_step_s);
            if !windows.is_empty() {
                requests.push(Request {
                    member: Member {
                        job,
                        slot: slot as u64,
                    },
                    windows,
                });
            }
        }
    }
    requests.sort_by(|a, b| {
        jobs[a.member.job]
            .aoi
            .key()
            .cmp(&jobs[b.member.job].aoi.key())
            .then(a.member.slot.cmp(&b.member.slot))
    });
    report.eligible_requests = requests.len();
    Ok(requests)
}

type Link = (usize, usize, f64, f64);
fn overlap_links(
    jobs: &[Job],
    atlas: &Atlas,
    requests: &[Request],
    policy: &Policy,
    report: &mut Report,
) -> Vec<Link> {
    let mut by_job = vec![Vec::new(); jobs.len()];
    for (i, request) in requests.iter().enumerate() {
        by_job[request.member.job].push(i);
    }
    let mut overlaps = BTreeMap::<(usize, usize), f64>::new();
    for patch in &atlas.patches {
        for (i, a) in patch.pieces.iter().enumerate() {
            if by_job[a.job].is_empty() {
                continue;
            }
            for j in patch.candidates(a.bounds) {
                let b = &patch.pieces[j];
                if j <= i || a.job == b.job || by_job[b.job].is_empty() {
                    continue;
                }
                let area = a.shape.intersection(&b.shape).unsigned_area();
                if area > 1e-9 {
                    *overlaps
                        .entry((a.job.min(b.job), a.job.max(b.job)))
                        .or_default() += area;
                }
            }
        }
    }
    let mut links = Vec::new();
    for ((a, b), area) in overlaps {
        let fraction = (area
            / atlas.job_areas_km2[a]
                .min(atlas.job_areas_km2[b])
                .max(1e-12))
        .min(1.0);
        if fraction < policy.min_overlap_fraction {
            continue;
        }
        report.overlapping_job_pairs += 1;
        for &i in &by_job[a] {
            for &j in &by_job[b] {
                if intersection(&requests[i].windows, &requests[j].windows).is_empty() {
                    continue;
                }
                links.push((i.min(j), i.max(j), fraction, area));
            }
        }
    }
    links.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| b.3.total_cmp(&a.3))
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
    });
    report.compatible_request_pairs = links.len();
    links
}

fn merge_requests(
    jobs: &[Job],
    atlas: &Atlas,
    requests: &[Request],
    links: Vec<Link>,
    context: &Context,
    policy: &Policy,
    report: &mut Report,
) -> Vec<Candidate> {
    let mut participating = vec![false; jobs.len()];
    for &(a, b, _, _) in &links {
        participating[requests[a].member.job] = true;
        participating[requests[b].member.job] = true;
    }
    let mut points = vec![Vec::new(); jobs.len()];
    for patch in &atlas.patches {
        for piece in &patch.pieces {
            if participating[piece.job] {
                for polygon in &piece.shape.0 {
                    points[piece.job]
                        .extend(polygon.exterior().0.iter().map(|&p| patch.chart.inverse(p)));
                }
            }
        }
    }
    let mut parents: Vec<_> = (0..requests.len()).collect();
    let mut groups: Vec<_> = requests
        .iter()
        .map(|r| Candidate {
            members: vec![r.member],
            windows: r.windows.clone(),
            estimate: None,
        })
        .collect();
    let mut bearings = BTreeMap::new();
    for (a, b, _, _) in links {
        let (a, b) = (root(&mut parents, a), root(&mut parents, b));
        if a == b {
            continue;
        }
        report.merge_attempts += 1;
        let mut members = groups[a].members.clone();
        members.extend_from_slice(&groups[b].members);
        if members.len() > policy.max_members {
            report.rejected_members += 1;
            continue;
        }
        if members.iter().map(|m| m.job).collect::<BTreeSet<_>>().len() != members.len() {
            report.rejected_repeated_job += 1;
            continue;
        }
        let windows = intersection(&groups[a].windows, &groups[b].windows);
        if windows.is_empty() {
            report.rejected_common_window += 1;
            continue;
        }
        let estimate = estimate_geometry(
            &members,
            &windows,
            jobs,
            &points,
            context,
            policy,
            &mut bearings,
        );
        if !estimate.accepted {
            report.rejected_size += 1;
            continue;
        }
        let (keep, drop) = (a.min(b), a.max(b));
        parents[drop] = keep;
        members.sort_by(|a, b| {
            jobs[a.job]
                .aoi
                .key()
                .cmp(&jobs[b.job].aoi.key())
                .then(a.slot.cmp(&b.slot))
        });
        groups[keep] = Candidate {
            members,
            windows,
            estimate: Some(estimate),
        };
        groups[drop].members.clear();
        groups[drop].windows.clear();
    }
    groups
}

fn union_groups(atlas: &Atlas, groups: Vec<Candidate>, job_count: usize) -> Vec<Group> {
    let mut out = Vec::new();
    let mut pieces_by_job = vec![Vec::new(); job_count];
    for (p, patch) in atlas.patches.iter().enumerate() {
        for (i, piece) in patch.pieces.iter().enumerate() {
            pieces_by_job[piece.job].push((p, i));
        }
    }
    for g in groups.into_iter().filter(|g| g.members.len() > 1) {
        let mut shapes = BTreeMap::<usize, Shape>::new();
        for member in &g.members {
            for &(p, i) in &pieces_by_job[member.job] {
                let shape = &atlas.patches[p].pieces[i].shape;
                shapes
                    .entry(p)
                    .and_modify(|s| *s = s.union(shape))
                    .or_insert_with(|| shape.clone());
            }
        }
        let union_area_km2 = shapes.values().map(Area::unsigned_area).sum();
        out.push(Group {
            id: out.len(),
            members: g.members,
            windows: g.windows,
            estimate: g.estimate.unwrap(),
            union_area_km2,
            pieces: shapes
                .into_iter()
                .map(|(patch, shape)| Piece { patch, shape })
                .collect(),
        });
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Context {
    pub epoch_unix_s: f64,
    pub horizon_s: f64,
    pub due_only: bool,
    pub swath_width_km: f64,
    pub strip_spacing_km: f64,
    pub boundary_margin_km: f64,
    pub max_strip_duration_s: f64,
}
impl Default for Context {
    fn default() -> Self {
        Self {
            epoch_unix_s: 1794528000.0,
            horizon_s: 86400.0,
            due_only: true,
            swath_width_km: 13.1,
            strip_spacing_km: 11.79,
            boundary_margin_km: 0.05,
            max_strip_duration_s: 600.0,
        }
    }
}
impl Context {
    pub fn validate(&self) -> Result<()> {
        if !self.epoch_unix_s.is_finite()
            || !self.horizon_s.is_finite()
            || self.horizon_s <= 0.0
            || self.horizon_s > 30.0 * 86400.0
            || !self.swath_width_km.is_finite()
            || self.swath_width_km <= 0.0
            || !self.boundary_margin_km.is_finite()
            || self.boundary_margin_km < 0.0
            || 2.0 * self.boundary_margin_km >= self.swath_width_km
            || !self.strip_spacing_km.is_finite()
            || self.strip_spacing_km <= 0.0
            || self.strip_spacing_km > self.swath_width_km
            || !self.max_strip_duration_s.is_finite()
            || self.max_strip_duration_s <= 0.0
        {
            return Err("Invalid grouper horizon or imaging geometry".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub min_overlap_fraction: f64,
    pub max_members: usize,
    pub max_span_km: f64,
    pub max_strips: usize,
    pub solar_step_s: f64,
    pub nominal_access_gap_hours: f64,
    pub strips_per_access: usize,
    pub access_fraction: f64,
    pub ground_speed_km_s: f64,
    pub maneuver_allowance_s: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            min_overlap_fraction: 0.0,
            max_members: 64,
            max_span_km: 150.0,
            max_strips: 8,
            solar_step_s: 60.0,
            nominal_access_gap_hours: 2.4,
            strips_per_access: 1,
            access_fraction: 1.0,
            ground_speed_km_s: 7.0,
            maneuver_allowance_s: 14.0,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if !self.min_overlap_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.min_overlap_fraction)
            || !(2..=1024).contains(&self.max_members)
            || !self.max_span_km.is_finite()
            || !(1.0..=1000.0).contains(&self.max_span_km)
            || !(1..=128).contains(&self.max_strips)
            || !self.solar_step_s.is_finite()
            || !(1.0..=300.0).contains(&self.solar_step_s)
            || !self.nominal_access_gap_hours.is_finite()
            || self.nominal_access_gap_hours <= 0.0
            || !(1..=16).contains(&self.strips_per_access)
            || !self.access_fraction.is_finite()
            || self.access_fraction <= 0.0
            || self.access_fraction > 1.0
            || !self.ground_speed_km_s.is_finite()
            || self.ground_speed_km_s <= 0.0
            || !self.maneuver_allowance_s.is_finite()
            || self.maneuver_allowance_s < 0.0
        {
            return Err("Invalid grouper size/window policy".into());
        }
        Ok(())
    }
}

pub struct EnrichedJobs<'a> {
    pub jobs: &'a [Job],
    pub groups: Vec<Group>,
    pub report: Report,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub job: usize,
    pub slot: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub start_s: f64,
    pub end_s: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: usize,
    pub members: Vec<Member>,
    pub windows: Vec<Window>,
    pub estimate: SizeEstimate,
    pub union_area_km2: f64,
    pub pieces: Vec<Piece>,
}
/// Polygon union in a chart of the original atlas; chart indices are not portable
/// to another atlas. The CLI/graph export writes the chart centers alongside it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Piece {
    pub patch: usize,
    pub shape: Shape,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SizeEstimate {
    pub accepted: bool,
    pub bearing_deg: f64,
    pub cross_track_km: f64,
    pub along_track_km: f64,
    pub span_km: f64,
    pub strips: usize,
    pub strip_duration_s: f64,
    pub usable_hours: f64,
    pub estimated_accesses: usize,
    pub strip_budget: usize,
}

/// Cheap admission estimate, independent of graph construction. One full swath
/// covers `width - 2*margin`; subsequent strips add the configured spacing.
/// Each usable interval contributes floor(hours * access_fraction / access_gap)
/// nominal accesses. A shorter interval admits one possible access if the strip
/// plus maneuver fits: this deliberately allows useful single-pass candidates,
/// but does not assert that a satellite is present. Disjoint intervals never
/// share a fractional access, and a strip must fit one contiguous interval.
pub fn size_constraint(
    cross_track_km: f64,
    along_track_km: f64,
    windows: &[Window],
    context: &Context,
    policy: &Policy,
) -> SizeEstimate {
    if context.validate().is_err()
        || policy.validate().is_err()
        || !cross_track_km.is_finite()
        || !along_track_km.is_finite()
        || cross_track_km < 0.0
        || along_track_km < 0.0
        || cross_track_km > policy.max_span_km
        || along_track_km > policy.max_span_km
        || windows.iter().any(|w| {
            !w.start_s.is_finite()
                || !w.end_s.is_finite()
                || w.start_s < 0.0
                || w.end_s > context.horizon_s
                || w.end_s <= w.start_s
        })
        || windows.windows(2).any(|w| w[0].end_s > w[1].start_s)
    {
        return SizeEstimate::default();
    }
    let width = context.swath_width_km - 2.0 * context.boundary_margin_km;
    let spacing = context.strip_spacing_km.min(width);
    let strips = 1 + ((cross_track_km - width).max(0.0) / spacing).ceil() as usize;
    let strip_duration_s = along_track_km / policy.ground_speed_km_s;
    let needed = strip_duration_s + policy.maneuver_allowance_s;
    let usable_hours = windows
        .iter()
        .map(|w| (w.end_s - w.start_s).max(0.0) / 3600.0)
        .sum();
    let estimated_accesses: usize = windows
        .iter()
        .map(|w| {
            let duration = w.end_s - w.start_s;
            if duration < needed {
                0
            } else {
                ((duration / 3600.0 * policy.access_fraction / policy.nominal_access_gap_hours)
                    .floor() as usize)
                    .max(1)
            }
        })
        .sum();
    let strip_budget = (estimated_accesses * policy.strips_per_access).min(policy.max_strips);
    let span_km = cross_track_km.hypot(along_track_km);
    SizeEstimate {
        accepted: cross_track_km.is_finite()
            && along_track_km.is_finite()
            && cross_track_km >= 0.0
            && along_track_km >= 0.0
            && span_km <= policy.max_span_km
            && strip_duration_s <= context.max_strip_duration_s
            && strips <= strip_budget,
        bearing_deg: 0.0,
        cross_track_km,
        along_track_km,
        span_km,
        strips,
        strip_duration_s,
        usable_hours,
        estimated_accesses,
        strip_budget,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Distribution {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub median: Option<f64>,
    pub mean: Option<f64>,
}
impl Distribution {
    fn new(values: impl Iterator<Item = f64>) -> Self {
        let mut v: Vec<_> = values.collect();
        v.sort_by(f64::total_cmp);
        if v.is_empty() {
            return Self::default();
        }
        Self {
            min: v.first().copied(),
            max: v.last().copied(),
            median: Some((v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.0),
            mean: Some(v.iter().sum::<f64>() / v.len() as f64),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub original_jobs: usize,
    pub requests_considered: usize,
    pub eligible_requests: usize,
    pub overlapping_job_pairs: usize,
    pub compatible_request_pairs: usize,
    pub merge_attempts: usize,
    pub rejected_members: usize,
    pub rejected_repeated_job: usize,
    pub rejected_common_window: usize,
    pub rejected_size: usize,
    pub groups_added: usize,
    pub grouped_requests: usize,
    pub grouped_jobs: usize,
    pub members_per_group: Distribution,
    pub strips_per_group: Distribution,
    pub common_window_hours: Distribution,
    pub window_s: f64,
    pub overlap_s: f64,
    pub merge_s: f64,
    pub union_s: f64,
    pub elapsed_s: f64,
}
struct Request {
    member: Member,
    windows: Vec<Window>,
}
struct Candidate {
    members: Vec<Member>,
    windows: Vec<Window>,
    estimate: Option<SizeEstimate>,
}
struct Solar {
    sun_fixed: Vector,
    greenwich_hours: f64,
}
fn solar_frames(context: &Context, policy: &Policy) -> Vec<[Solar; 3]> {
    (0..(context.horizon_s / policy.solar_step_s).ceil() as usize)
        .map(|i| {
            [0.0, 0.5, 1.0].map(|f| {
                let instant = context.epoch_unix_s
                    + ((i as f64 + f) * policy.solar_step_s).min(context.horizon_s);
                let sun = sun_direction(instant);
                let rotation = gmst(instant);
                let (s, c) = rotation.sin_cos();
                Solar {
                    sun_fixed: [c * sun[0] - s * sun[2], sun[1], s * sun[0] + c * sun[2]],
                    greenwich_hours: local_solar_time_hours(
                        ground_position(0.0, 0.0, 1.0, rotation),
                        sun,
                    ),
                }
            })
        })
        .collect()
}
fn usable_windows(job: &Job, window: Window, frames: &[[Solar; 3]], step: f64) -> Vec<Window> {
    if job.acquisition.illumination == Illumination::AnyTime {
        return vec![window];
    }
    let anchor = ground_position(job.aoi.latitude, job.aoi.longitude, 1.0, 0.0);
    let mut out: Vec<Window> = Vec::new();
    let first = (window.start_s / step).floor() as usize;
    let last = (window.end_s / step).ceil() as usize;
    for (i, samples) in frames.iter().enumerate().take(last).skip(first) {
        if !samples.iter().all(|s| {
            job.acquisition.accepts(
                (s.greenwich_hours + job.aoi.longitude / 15.0).rem_euclid(24.0),
                dot(anchor, s.sun_fixed)
                    .clamp(-1.0, 1.0)
                    .asin()
                    .to_degrees(),
            )
        }) {
            continue;
        }
        let next = Window {
            start_s: window.start_s.max(i as f64 * step),
            end_s: window.end_s.min((i + 1) as f64 * step),
        };
        if let Some(last) = out
            .last_mut()
            .filter(|w| (w.end_s - next.start_s).abs() < 1e-8)
        {
            last.end_s = next.end_s;
        } else {
            out.push(next);
        }
    }
    out
}
fn intersection(a: &[Window], b: &[Window]) -> Vec<Window> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        let start_s = a[i].start_s.max(b[j].start_s);
        let end_s = a[i].end_s.min(b[j].end_s);
        if end_s > start_s {
            out.push(Window { start_s, end_s });
        }
        if a[i].end_s <= b[j].end_s {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}
fn root(parents: &mut [usize], mut i: usize) -> usize {
    while parents[i] != i {
        parents[i] = parents[parents[i]];
        i = parents[i];
    }
    i
}
fn estimate_geometry(
    members: &[Member],
    windows: &[Window],
    jobs: &[Job],
    points: &[Vec<Vector>],
    context: &Context,
    policy: &Policy,
    cache: &mut BTreeMap<i16, [f64; 2]>,
) -> SizeEstimate {
    let sum = members.iter().fold([0.0; 3], |sum, m| {
        let p = ground_position(
            jobs[m.job].aoi.latitude,
            jobs[m.job].aoi.longitude,
            1.0,
            0.0,
        );
        std::array::from_fn(|k| sum[k] + p[k])
    });
    if leo_sim::geometry::norm(sum) < 1e-9 {
        return SizeEstimate::default();
    }
    let origin = geometry::unit(sum);
    let lat = origin[1].clamp(-1.0, 1.0).asin().to_degrees();
    let lon = (-origin[2]).atan2(origin[0]).to_degrees();
    let east = ground_position(0.0, lon + 90.0, 1.0, 0.0);
    let north = geometry::cross(origin, east);
    let headings = *cache
        .entry(lat.round() as i16)
        .or_insert_with(|| crate::admission::bearings(lat.round(), context.epoch_unix_s));
    let mut choices = Vec::new();
    for bearing in headings {
        let along: Vector =
            std::array::from_fn(|k| east[k] * bearing.sin() + north[k] * bearing.cos());
        let across: Vector =
            std::array::from_fn(|k| east[k] * bearing.cos() - north[k] * bearing.sin());
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        let mut far = false;
        for m in members {
            for &p in &points[m.job] {
                // Local tangent projection is only an estimate; reject distant points
                // instead of letting orthographic folding hide a huge polygon.
                far |= dot(origin, p) < (policy.max_span_km / R).cos();
                for (k, axis) in [across, along].into_iter().enumerate() {
                    let v = dot(p, axis) * R;
                    lo[k] = lo[k].min(v);
                    hi[k] = hi[k].max(v);
                }
            }
        }
        let mut estimate = size_constraint(hi[0] - lo[0], hi[1] - lo[1], windows, context, policy);
        estimate.bearing_deg = bearing.to_degrees();
        estimate.accepted &= !far;
        choices.push(estimate);
    }
    choices.sort_by(|a, b| {
        b.accepted
            .cmp(&a.accepted)
            .then(a.strips.cmp(&b.strips))
            .then_with(|| a.strip_duration_s.total_cmp(&b.strip_duration_s))
    });
    choices.remove(0)
}
