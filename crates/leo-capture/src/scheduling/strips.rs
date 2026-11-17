//! AOI-aligned full-strip candidates with optional local task clustering.
//! The sampling step is used for geometry integration, never to split graph actions.
mod project;
#[cfg(test)]
mod tests;

use super::{
    export::{self, CropRecord, SatelliteSummary, SolarMasks},
    intervals, Action, Config, Mode,
};
use crate::{
    baseline,
    geometry::{self, Atlas, Shape},
    resources,
    workload::Job,
};
use geo::{Area, BooleanOps, BoundingRect, Coord, MultiPolygon, Rect};
use leo_sim::{geometry::EARTH_RADIUS_KM as R, orbit::Elements};
use project::Track;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clustering {
    /// Limit local pair proposals; never merge an entire transitive overlap component.
    pub neighbors: usize,
    pub max_gap_s: f64,
    pub max_span_s: f64,
    pub max_width_swaths: f64,
}
impl Default for Clustering {
    fn default() -> Self {
        Self {
            neighbors: 4,
            max_gap_s: 3.0,
            max_span_s: 60.0,
            max_width_swaths: 4.0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    pub access_step_s: f64,
    /// Guard against projection/edge interpolation error at outer mosaic boundaries.
    pub boundary_margin_km: f64,
    pub time_margin_s: f64,
    pub max_duration_s: f64,
    pub clustering: Option<Clustering>,
    /// Preprocess shared AOIs once, before per-satellite strip construction.
    pub grouping: Option<crate::job_grouper::Policy>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            access_step_s: 30.0,
            boundary_margin_km: 0.05,
            time_margin_s: 0.02,
            max_duration_s: 600.0,
            clustering: None,
            grouping: None,
        }
    }
}
impl Options {
    pub(super) fn validate(&self, imaging: &super::Options) -> Result<()> {
        if !self.access_step_s.is_finite()
            || !(1.0..=60.0).contains(&self.access_step_s)
            || !self.boundary_margin_km.is_finite()
            || !(0.001..=0.5).contains(&self.boundary_margin_km)
            || imaging.width_km <= 2.0 * self.boundary_margin_km
            || !self.time_margin_s.is_finite()
            || !(0.001..=1.0).contains(&self.time_margin_s)
            || !self.max_duration_s.is_finite()
            || !(1.0..=1800.0).contains(&self.max_duration_s)
        {
            return Err("Invalid AOI strip construction settings".into());
        }
        if let Some(c) = &self.clustering {
            if c.neighbors == 0
                || c.neighbors > 32
                || !c.max_gap_s.is_finite()
                || !(0.0..=60.0).contains(&c.max_gap_s)
                || !c.max_span_s.is_finite()
                || c.max_span_s <= 0.0
                || c.max_span_s > self.max_duration_s
                || !c.max_width_swaths.is_finite()
                || !(1.0..=16.0).contains(&c.max_width_swaths)
            {
                return Err("Invalid strip clustering settings".into());
            }
        }
        if let Some(policy) = &self.grouping {
            policy.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Statistics {
    pub access_windows: usize,
    pub projected_targets: usize,
    pub base_proposals: usize,
    pub clustered_pairs: usize,
    pub refinement_proposals: usize,
    pub base_actions: usize,
    pub refinement_actions: usize,
    pub group_proposals: usize,
    pub group_actions: usize,
    pub shared_actions: usize,
    pub duration_min_s: Option<f64>,
    pub duration_max_s: Option<f64>,
    pub duration_sum_s: f64,
    pub base_generation_s: f64,
    pub refinement_s: f64,
    pub polygon_overlay_s: f64,
    pub topology_s: f64,
}
struct Target {
    job: usize,
    along: f64,
    shape: Shape,
    bounds: Rect<f64>,
}
#[derive(Clone)]
struct Proposal {
    start: f64,
    end: f64,
    along: f64,
    cross: f64,
    seeds: Vec<usize>,
    refinement: bool,
    group: Option<usize>,
}
impl Proposal {
    fn key(&self) -> (i64, i64, i64, i64) {
        (
            (self.start * 1000.0).round() as i64,
            (self.end * 1000.0).round() as i64,
            (self.along * 1e6).round() as i64,
            (self.cross * 1e6).round() as i64,
        )
    }
}
fn layouts(
    shape: &Shape,
    along: f64,
    seeds: &[usize],
    refinement: bool,
    config: &Config,
) -> Vec<Proposal> {
    let Some(bounds) = shape.bounding_rect() else {
        return Vec::new();
    };
    let options = config.strips.as_ref().unwrap();
    let width = config.options.width_km;
    let usable_width = width - 2.0 * options.boundary_margin_km;
    let spacing = config.options.cross_spacing_km.min(usable_width);
    let count = (((bounds.height() - usable_width).max(0.0) / spacing).ceil() as usize) + 1;
    let mut out = Vec::new();
    for right in [false, true] {
        for i in 0..count {
            let cross = if right {
                bounds.max().y - usable_width / 2.0 - i as f64 * spacing
            } else {
                bounds.min().y + usable_width / 2.0 + i as f64 * spacing
            };
            let cross = (cross * 1e6).round() / 1e6;
            if along.hypot(cross) > (config.options.reach_km - width) / 2.0 {
                continue;
            }
            let band = Rect::new(
                Coord {
                    x: bounds.min().x - 1.0,
                    y: cross - width / 2.0,
                },
                Coord {
                    x: bounds.max().x + 1.0,
                    y: cross + width / 2.0,
                },
            )
            .to_polygon();
            let clipped = shape.intersection(&MultiPolygon::new(vec![band]));
            let Some(span) = clipped.bounding_rect() else {
                continue;
            };
            let start = ((span.min().x - options.time_margin_s) * 1000.0).floor() / 1000.0;
            let end = ((span.max().x + options.time_margin_s) * 1000.0).ceil() / 1000.0;
            if start < 0.0
                || end > config.options.duration_s
                || end <= start
                || end - start > options.max_duration_s
            {
                continue;
            }
            out.push(Proposal {
                start,
                end,
                along,
                cross,
                seeds: seeds.to_vec(),
                refinement,
                group: None,
            });
        }
    }
    out
}
fn windows_overlap(a: &Job, b: &Job, start: f64, end: f64) -> bool {
    let windows = |j: &Job| {
        let first = ((start - j.acquisition.window_origin_s(j.cadence_hours))
            / (j.cadence_hours * 3600.0))
            .floor()
            .max(0.0) as u64;
        let last = ((end - j.acquisition.window_origin_s(j.cadence_hours))
            / (j.cadence_hours * 3600.0))
            .floor()
            .max(0.0) as u64;
        (first..=last)
            .map(|slot| j.acquisition.collection_window_s(slot, j.cadence_hours))
            .collect::<Vec<_>>()
    };
    windows(a).iter().any(|&(a0, a1)| {
        windows(b)
            .iter()
            .any(|&(b0, b1)| a0.max(b0).max(start) < a1.min(b1).min(end))
    })
}
fn refine(
    targets: &[Target],
    jobs: &[Job],
    config: &Config,
    stats: &mut Statistics,
) -> Vec<Proposal> {
    let Some(options) = &config.strips.as_ref().unwrap().clustering else {
        return Vec::new();
    };
    use rstar::{
        primitives::{GeomWithData, Rectangle},
        RTree, AABB,
    };
    let tree = RTree::bulk_load(
        targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                GeomWithData::new(
                    Rectangle::from_corners(
                        [t.bounds.min().x, t.bounds.min().y],
                        [t.bounds.max().x, t.bounds.max().y],
                    ),
                    i,
                )
            })
            .collect(),
    );
    let mut pairs = BTreeSet::new();
    let mut out = Vec::new();
    for (i, a) in targets.iter().enumerate() {
        let width = config.options.width_km;
        let query = AABB::from_corners(
            [
                a.bounds.min().x - options.max_gap_s,
                a.bounds.min().y - width,
            ],
            [
                a.bounds.max().x + options.max_gap_s,
                a.bounds.max().y + width,
            ],
        );
        let mut nearby: Vec<_> = tree
            .locate_in_envelope_intersecting(&query)
            .map(|v| v.data)
            .filter(|&j| i != j && targets[j].job != a.job && targets[j].along == a.along)
            .collect();
        nearby.sort_by(|&j, &k| {
            let distance = |t: &Target| {
                (t.bounds.center().x - a.bounds.center().x).abs()
                    + (t.bounds.center().y - a.bounds.center().y).abs() / 7.0
            };
            distance(&targets[j])
                .total_cmp(&distance(&targets[k]))
                .then(j.cmp(&k))
        });
        let mut accepted = 0;
        for j in nearby {
            let b = &targets[j];
            let lo = a.bounds.min().x.min(b.bounds.min().x);
            let hi = a.bounds.max().x.max(b.bounds.max().x);
            if hi - lo > options.max_span_s
                || a.bounds.max().y.max(b.bounds.max().y) - a.bounds.min().y.min(b.bounds.min().y)
                    > options.max_width_swaths * width
                || !windows_overlap(&jobs[a.job], &jobs[b.job], lo, hi)
            {
                continue;
            }
            if !pairs.insert((i.min(j), i.max(j))) {
                continue;
            }
            let union = a.shape.union(&b.shape);
            out.extend(layouts(&union, a.along, &[a.job, b.job], true, config));
            stats.clustered_pairs += 1;
            accepted += 1;
            if accepted == options.neighbors {
                break;
            }
        }
    }
    out
}

fn overlay(
    proposal: &Proposal,
    track: &Track,
    config: &Config,
    jobs: &[Job],
    atlas: &Atlas,
    satellite: usize,
) -> Result<Vec<CropRecord>> {
    let mode = Mode::new(proposal.along, proposal.cross);
    let mut merged: BTreeMap<(usize, u64, usize), (f64, f64, Shape)> = BTreeMap::new();
    let mut start = proposal.start;
    let options = config.options.baseline();
    while start < proposal.end - 1e-9 {
        let end = (((start / options.step_s).floor() + 1.0) * options.step_s)
            .min(proposal.end)
            .max((start + 1e-6).min(proposal.end));
        let (q, n) = track.at(start);
        let (a, na) = mode.offset(q, n);
        let (q, n) = track.at(end);
        let (b, nb) = mode.offset(q, n);
        let segment = baseline::segment(
            jobs,
            atlas,
            config.epoch_unix_s,
            config.options.width_km / (2.0 * R),
            &options,
            satellite,
            start,
            end,
            a,
            na,
            b,
            nb,
            None,
        )?;
        for op in segment.ops {
            for crop in op.crops {
                let key = (crop.job, crop.slot, op.patch);
                if let Some((lo, hi, shape)) = merged.get_mut(&key) {
                    *lo = lo.min(crop.start_s);
                    *hi = hi.max(crop.end_s);
                    *shape = shape.union(&crop.geometry);
                } else {
                    merged.insert(key, (crop.start_s, crop.end_s, crop.geometry));
                }
            }
        }
        start = end;
    }
    Ok(merged
        .into_iter()
        .map(|((job, slot, patch), (start_s, end_s, shape))| CropRecord {
            job,
            slot,
            patch,
            start_s,
            end_s,
            area_km2: shape.unsigned_area(),
            polygons_xy_km: geometry::coordinates(&shape),
        })
        .collect())
}

pub(super) fn satellite(
    config: &Config,
    jobs: &[Job],
    atlas: &Atlas,
    masks: &SolarMasks,
    groups: &[crate::job_grouper::Group],
    id: usize,
    elements: Elements,
) -> Result<SatelliteSummary> {
    resources::background_thread();
    let started = Instant::now();
    let track = Track::new(
        &elements,
        config.epoch_unix_s,
        config.options.duration_s,
        config.options.step_s,
    );
    let mut stats = Statistics::default();
    let (targets, passes) = project::targets(config, jobs, atlas, masks, &track, &mut stats)?;
    let mut proposals = Vec::new();
    for t in &targets {
        proposals.extend(layouts(&t.shape, t.along, &[t.job], false, config));
    }
    stats.base_proposals = proposals.len();
    stats.base_generation_s = started.elapsed().as_secs_f64();
    let refining = Instant::now();
    let extra = refine(&targets, jobs, config, &mut stats);
    stats.refinement_proposals = extra.len();
    proposals.extend(extra);
    let grouped = project::group_proposals(config, groups, atlas, &passes, &track);
    stats.group_proposals = grouped.len();
    proposals.extend(grouped);
    stats.refinement_s = refining.elapsed().as_secs_f64();
    drop(targets);
    // Base proposals win identical physical identities. This preserves their
    // geometry under the separate optional refinement phase; chronological IDs may change.
    let mut unique = BTreeMap::new();
    for p in proposals {
        unique.entry(p.key()).or_insert(p);
    }
    let mut modes = Vec::new();
    let mut mode_ids = BTreeMap::new();
    let mut actions = Vec::new();
    let mut job_hits = vec![false; jobs.len()];
    let mut crop_count = 0;
    let target = config
        .output
        .join(format!("satellite-{id:03}.actions.jsonl.gz"));
    let part = target.with_extension("gz.part");
    let mut writer = export::writer(&part)?;
    let overlaying = Instant::now();
    let mut last = Instant::now();
    let total = unique.len();
    for (i, (_, p)) in unique.into_iter().enumerate() {
        let crops = overlay(&p, &track, config, jobs, atlas, id)?;
        if crops.is_empty()
            || config.only_actions_serving_due_deadlines
                && !crops
                    .iter()
                    .any(|c| c.slot < masks.due_counts[c.job] as u64)
        {
            continue;
        }
        if p.refinement
            && p.group.is_none()
            && !p.seeds.iter().all(|j| {
                crops.iter().any(|c| {
                    c.job == *j
                        && (!config.only_actions_serving_due_deadlines
                            || c.slot < masks.due_counts[c.job] as u64)
                })
            })
        {
            continue;
        }
        if let Some(g) = p.group {
            // A mosaic's strips serve subsets of the group. Preserve the exact
            // original request identity; another cycle cannot satisfy this one.
            if !crops.iter().any(|c| {
                groups[g]
                    .members
                    .iter()
                    .any(|m| m.job == c.job && m.slot == c.slot)
            }) {
                continue;
            }
        }
        let key = (
            (p.along * 1e6).round() as i64,
            (p.cross * 1e6).round() as i64,
        );
        let mode = *mode_ids.entry(key).or_insert_with(|| {
            let index = modes.len();
            modes.push(Mode::new(p.along, p.cross));
            index
        });
        let action = Action {
            tick: (p.start / config.options.step_s).floor() as u32,
            mode,
            start_s: p.start,
            end_s: p.end,
        };
        let a = actions.len();
        export::line(
            &mut writer,
            &serde_json::json!({"id":a,"tick":action.tick,"mode":mode,"start_s":p.start,"end_s":p.end,"origin":if p.group.is_some() {"group"} else if p.refinement {"cluster"} else {"aoi"},"group":p.group,"seed_jobs":p.seeds,"crops":crops}),
        )?;
        let mut served = BTreeSet::new();
        for c in &crops {
            job_hits[c.job] = true;
            served.insert(c.job);
        }
        stats.shared_actions += usize::from(served.len() > 1);
        stats.base_actions += usize::from(!p.refinement);
        stats.refinement_actions += usize::from(p.refinement && p.group.is_none());
        stats.group_actions += usize::from(p.group.is_some());
        let duration = p.end - p.start;
        stats.duration_min_s = Some(stats.duration_min_s.map_or(duration, |x| x.min(duration)));
        stats.duration_max_s = Some(stats.duration_max_s.map_or(duration, |x| x.max(duration)));
        stats.duration_sum_s += duration;
        crop_count += crops.len() as u64;
        actions.push(action);
        if i % 64 == 0 {
            resources::check_memory()?;
            export::check_disk(&config.output)?;
        }
        if last.elapsed().as_secs() >= 20 {
            eprintln!(
                "AOI strips satellite {id}: {i}/{total} candidates, {} accepted",
                actions.len()
            );
            last = Instant::now();
        }
    }
    let actions_bytes = export::finish(writer, &part, &target)?;
    stats.polygon_overlay_s = overlaying.elapsed().as_secs_f64();
    let (nodes, edges, maneuvers, graph_bytes, unreachable, topology_s) =
        intervals::write(config, id, &actions, &modes)?;
    stats.topology_s = topology_s;
    eprintln!(
        "AOI strips satellite {id}: {} base + {} paired + {} group actions, {} shared, {edges} edges",
        stats.base_actions, stats.refinement_actions, stats.group_actions, stats.shared_actions
    );
    Ok(SatelliteSummary {
        satellite: id,
        modes,
        strip_generation: Some(stats),
        actions: actions.len() as u64,
        nodes,
        edges,
        maneuver_edges: maneuvers,
        crop_records: crop_count,
        jobs_with_candidates: job_hits.iter().filter(|&&x| x).count(),
        source_unreachable_actions: unreachable,
        actions_bytes,
        graph_bytes,
        elapsed_s: started.elapsed().as_secs_f64(),
        observed_process_memory_bytes: resources::process_memory().unwrap_or(0),
        job_hits,
    })
}
