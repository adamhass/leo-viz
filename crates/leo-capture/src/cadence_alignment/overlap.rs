use super::*;
use crate::workload::{Fulfilment, Illumination, MosaicTiming};
use geo::{Area, BooleanOps};
use leo_sim::geometry::{ground_position, EARTH_RADIUS_KM};

pub(super) struct Edge {
    pub a: usize,
    pub b: usize,
    pub area: f64,
    fraction: f64,
}

fn compatible(a: &Job, b: &Job) -> bool {
    if a.cadence_hours != b.cadence_hours {
        return false;
    }
    let tolerance = |j: &Job| {
        if j.acquisition.illumination == Illumination::AnyTime
            || j.acquisition.fulfilment == Fulfilment::Mosaic
                && j.acquisition.mosaic_timing == MosaicTiming::CloseInTime
        {
            12.0
        } else {
            j.acquisition.solar_tolerance_hours
        }
    };
    let distance = ((a.acquisition.solar_time_hours - b.acquisition.solar_time_hours + 12.0)
        .rem_euclid(24.0)
        - 12.0)
        .abs();
    distance <= tolerance(a) + tolerance(b)
}

pub(super) fn edges(jobs: &[Job], atlas: &Atlas, policy: &Policy) -> Result<Vec<Edge>> {
    let records = crate::parallel::map(
        atlas.patches.iter().collect(),
        crate::parallel::worker_count(atlas.patches.len()),
        |patch| {
            let mut pairs = Vec::new();
            for (i, a) in patch.pieces.iter().enumerate() {
                for j in patch.candidates(a.bounds) {
                    if j <= i {
                        continue;
                    }
                    let b = &patch.pieces[j];
                    if a.job == b.job || !compatible(&jobs[a.job], &jobs[b.job]) {
                        continue;
                    }
                    let area = a.shape.intersection(&b.shape).unsigned_area();
                    if area > 1e-9 {
                        pairs.push(((a.job.min(b.job), a.job.max(b.job)), area));
                    }
                }
            }
            Ok(pairs)
        },
        |n, total| {
            if n == total {
                eprintln!("Indexed overlaps across {total} geographic charts");
            }
        },
    )?;
    let mut combined = BTreeMap::<(usize, usize), f64>::new();
    for patch in records {
        for (pair, area) in patch {
            *combined.entry(pair).or_default() += area;
        }
    }
    let mut out: Vec<_> = combined
        .into_iter()
        .filter_map(|((a, b), area)| {
            let fraction = (area
                / atlas.job_areas_km2[a]
                    .min(atlas.job_areas_km2[b])
                    .max(1e-12))
            .min(1.0);
            (fraction >= policy.min_overlap_fraction).then_some(Edge {
                a,
                b,
                area,
                fraction,
            })
        })
        .collect();
    // Canonical endpoint order makes both grouping and ties independent of array order.
    for e in &mut out {
        if jobs[e.a].aoi.key() > jobs[e.b].aoi.key() {
            std::mem::swap(&mut e.a, &mut e.b);
        }
    }
    out.sort_by(|a, b| {
        b.fraction
            .total_cmp(&a.fraction)
            .then_with(|| b.area.total_cmp(&a.area))
            .then_with(|| jobs[a.a].aoi.key().cmp(&jobs[b.a].aoi.key()))
            .then_with(|| jobs[a.b].aoi.key().cmp(&jobs[b.b].aoi.key()))
    });
    Ok(out)
}

pub(super) fn groups(jobs: &[Job], edges: &[Edge], policy: &Policy) -> Vec<Vec<usize>> {
    let mut parents: Vec<_> = (0..jobs.len()).collect();
    let mut sizes = vec![1; jobs.len()];
    let mut lo: Vec<_> = jobs
        .iter()
        .map(|j| ground_position(j.aoi.latitude, j.aoi.longitude, EARTH_RADIUS_KM, 0.0))
        .collect();
    let mut hi = lo.clone();
    fn root(parents: &mut [usize], mut i: usize) -> usize {
        while parents[i] != i {
            parents[i] = parents[parents[i]];
            i = parents[i];
        }
        i
    }
    for e in edges {
        let a = root(&mut parents, e.a);
        let b = root(&mut parents, e.b);
        if a == b || sizes[a] + sizes[b] > policy.max_group_jobs {
            continue;
        }
        let min: [f64; 3] = std::array::from_fn(|i| lo[a][i].min(lo[b][i]));
        let max: [f64; 3] = std::array::from_fn(|i| hi[a][i].max(hi[b][i]));
        let span = (0..3)
            .map(|i| (max[i] - min[i]).powi(2))
            .sum::<f64>()
            .sqrt();
        if span > policy.max_group_span_km {
            continue;
        }
        parents[b] = a;
        sizes[a] += sizes[b];
        lo[a] = min;
        hi[a] = max;
    }
    let mut groups = BTreeMap::<usize, Vec<usize>>::new();
    for i in 0..jobs.len() {
        groups.entry(root(&mut parents, i)).or_default().push(i);
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    for group in &mut groups {
        group.sort_by_key(|&i| jobs[i].aoi.key());
    }
    groups.sort_by_key(|g| jobs[g[0]].aoi.key());
    groups
}
