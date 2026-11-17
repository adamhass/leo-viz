//! Optional, demand-preserving assignment of ongoing deadlines to geographic jobs.
//! This changes phase only: the multiset of deadlines within each cadence is fixed.
pub mod export;
mod overlap;
#[cfg(test)]
mod tests;

use crate::{
    geometry::{Atlas, Region},
    workload::{hash, Job},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Instant};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    LongitudeBands,
    #[default]
    OverlapBalanced,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DailyLoadTarget {
    PerJob,
    #[default]
    OriginalDemand,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WithinDayOrder {
    Construction,
    #[default]
    SolarWindow,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub method: Method,
    pub seed: u64,
    pub longitude_origin_deg: f64,
    pub min_overlap_fraction: f64,
    pub max_group_jobs: usize,
    pub max_group_span_km: f64,
    pub balance_cell_deg: f64,
    pub local_balance_weight: f64,
    pub daily_load_target: DailyLoadTarget,
    pub within_day_order: WithinDayOrder,
    pub priority_base: f64,
    pub report_days: usize,
    pub cell_deg: f64,
    pub edge_km: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            method: Method::OverlapBalanced,
            seed: 20261113,
            longitude_origin_deg: -180.0,
            min_overlap_fraction: 0.05,
            max_group_jobs: 64,
            max_group_span_km: 100.0,
            balance_cell_deg: 5.0,
            local_balance_weight: 0.25,
            daily_load_target: DailyLoadTarget::OriginalDemand,
            within_day_order: WithinDayOrder::SolarWindow,
            priority_base: 2.0,
            report_days: 30,
            cell_deg: 2.0,
            edge_km: 2.0,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if !self.longitude_origin_deg.is_finite()
            || !self.min_overlap_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.min_overlap_fraction)
            || !(2..=1024).contains(&self.max_group_jobs)
            || !self.max_group_span_km.is_finite()
            || !(1.0..=1000.0).contains(&self.max_group_span_km)
            || !self.balance_cell_deg.is_finite()
            || !(0.25..=30.0).contains(&self.balance_cell_deg)
            || !self.local_balance_weight.is_finite()
            || !(0.0..=10.0).contains(&self.local_balance_weight)
            || !self.priority_base.is_finite()
            || !(1.0..=16.0).contains(&self.priority_base)
            || !(1..=366).contains(&self.report_days)
        {
            return Err("Invalid cadence alignment policy".into());
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Assignment {
    pub job_key: String,
    pub cadence_hours: f64,
    pub original_deadline_hours: f64,
    pub aligned_deadline_hours: f64,
    pub overlap_group: usize,
}
pub struct Alignment {
    pub jobs: Vec<Job>,
    pub assignments: Vec<Assignment>,
    pub report: serde_json::Value,
}

fn deadline(job: &Job) -> f64 {
    job.acquisition.first_deadline_hours.unwrap()
}
// A deadline exactly at midnight is due in the day which just ended.
fn day(hours: f64) -> usize {
    (hours / 24.0).ceil().max(1.0) as usize - 1
}
fn cell(job: &Job, width: f64) -> (i32, i32) {
    (
        ((job.aoi.longitude + 180.0).rem_euclid(360.0) / width).floor() as i32,
        ((job.aoi.latitude + 90.0) / width).floor() as i32,
    )
}
fn weight(job: &Job, area: f64, base: f64) -> f64 {
    area * base.powi(job.acquisition.priority as i32)
}
fn tie(jobs: &[Job], i: usize, seed: u64) -> u64 {
    hash(jobs[i].aoi.key().as_bytes()) ^ seed
}

fn daily(jobs: &[Job], areas: &[f64], policy: &Policy) -> Vec<serde_json::Value> {
    let mut counts = vec![0_usize; policy.report_days];
    let mut area = vec![0.0; policy.report_days];
    let mut reward = vec![0.0; policy.report_days];
    for (i, job) in jobs.iter().enumerate() {
        let mut end = deadline(job);
        while end <= policy.report_days as f64 * 24.0 {
            let d = day(end);
            counts[d] += 1;
            area[d] += areas[i];
            reward[d] += weight(job, areas[i], policy.priority_base);
            end += job.cadence_hours;
        }
    }
    (0..policy.report_days)
        .map(|d| {
            serde_json::json!({"day":d,"due_deadlines":counts[d],
        "available_area_km2":area[d],"available_priority_reward":reward[d]})
        })
        .collect()
}

fn overlap_metrics(jobs: &[Job], edges: &[overlap::Edge]) -> serde_json::Value {
    let mut aligned = 0;
    let mut aligned_area = 0.0;
    let mut total_area = 0.0;
    let mut gap = 0.0;
    for e in edges {
        let a = deadline(&jobs[e.a]);
        let b = deadline(&jobs[e.b]);
        let distance = (a - b).abs();
        gap += distance.min(jobs[e.a].cadence_hours - distance) * e.area;
        total_area += e.area;
        if day(a) == day(b) {
            aligned += 1;
            aligned_area += e.area;
        }
    }
    serde_json::json!({"eligible_overlap_pairs":edges.len(),"same_day_pairs":aligned,
        "pair_overlap_area_km2":total_area,"same_day_overlap_area_km2":aligned_area,
        "same_day_overlap_area_percent":if total_area>0.0 {100.0*aligned_area/total_area} else {0.0},
        "overlap_weighted_circular_phase_gap_hours":if total_area>0.0 {gap/total_area} else {0.0}})
}

/// Assign a fresh deadline permutation without modifying the caller or other job fields.
pub fn align(jobs: &[Job], regions: &[Region], policy: &Policy) -> Result<Alignment> {
    policy.validate()?;
    if jobs.is_empty() {
        return Err("No jobs to align".into());
    }
    let started = Instant::now();
    let mut keys = std::collections::BTreeSet::new();
    for job in jobs {
        job.validate()?;
        if !keys.insert(job.aoi.key()) {
            return Err("Duplicate alignment job key".into());
        }
        if job.acquisition.first_deadline_hours.is_none() {
            return Err("Cadence alignment requires ongoing jobs with explicit next deadlines; run leo-job-deadlines first".into());
        }
        if !weight(job, job.aoi.area_km2, policy.priority_base).is_finite() {
            return Err("Non-finite alignment reward weight".into());
        }
    }
    eprintln!("Building polygon overlap index for {} jobs", jobs.len());
    let atlas = Atlas::new(jobs, regions, policy.cell_deg, policy.edge_km, |_| true)?;
    let edges = overlap::edges(jobs, &atlas, policy)?;
    let geometry_s = started.elapsed().as_secs_f64();
    let groups = if policy.method == Method::OverlapBalanced {
        overlap::groups(jobs, &edges, policy)
    } else {
        // Stable identifiers even when the input job array is reordered.
        let mut order: Vec<_> = (0..jobs.len()).collect();
        order.sort_by_key(|&i| jobs[i].aoi.key());
        order.into_iter().map(|i| vec![i]).collect()
    };
    let mut cadence_groups = BTreeMap::<u64, Vec<usize>>::new();
    for (g, members) in groups.iter().enumerate() {
        cadence_groups
            .entry(jobs[members[0]].cadence_hours.to_bits())
            .or_default()
            .push(g);
    }
    let mut aligned = jobs.to_vec();
    let mut split_groups = 0;
    for group_ids in cadence_groups.values() {
        let ids: Vec<_> = group_ids
            .iter()
            .flat_map(|&g| groups[g].iter().copied())
            .collect();
        let mut deadlines: Vec<_> = ids.iter().map(|&i| deadline(&jobs[i])).collect();
        deadlines.sort_by(f64::total_cmp);
        if policy.method == Method::LongitudeBands {
            let mut order = ids;
            order.sort_by(|&a, &b| {
                let longitude = |i: usize| {
                    (jobs[i].aoi.longitude - policy.longitude_origin_deg).rem_euclid(360.0)
                };
                longitude(a)
                    .total_cmp(&longitude(b))
                    .then_with(|| jobs[a].aoi.key().cmp(&jobs[b].aoi.key()))
            });
            for (i, d) in order.into_iter().zip(deadlines) {
                aligned[i].acquisition.first_deadline_hours = Some(d);
            }
        } else {
            split_groups += balance(
                jobs,
                &atlas.job_areas_km2,
                &groups,
                group_ids,
                &deadlines,
                policy,
                &mut aligned,
            );
        }
    }
    if policy.within_day_order == WithinDayOrder::SolarWindow {
        order_within_days(jobs, &groups, &mut aligned);
    }
    export::verify_permutation(jobs, &aligned)?;
    let mut group_of = vec![0; jobs.len()];
    for (g, ids) in groups.iter().enumerate() {
        for &i in ids {
            group_of[i] = g;
        }
    }
    let assignments = jobs
        .iter()
        .zip(&aligned)
        .enumerate()
        .map(|(i, (a, b))| Assignment {
            job_key: a.aoi.key(),
            cadence_hours: a.cadence_hours,
            original_deadline_hours: deadline(a),
            aligned_deadline_hours: deadline(b),
            overlap_group: group_of[i],
        })
        .collect();
    let before = daily(jobs, &atlas.job_areas_km2, policy);
    let after = daily(&aligned, &atlas.job_areas_km2, policy);
    if before
        .iter()
        .zip(&after)
        .any(|(a, b)| a["due_deadlines"] != b["due_deadlines"])
    {
        return Err("Alignment changed daily demand counts".into());
    }
    let report = serde_json::json!({"policy":policy,"jobs":jobs.len(),"geometry_s":geometry_s,
        "elapsed_s":started.elapsed().as_secs_f64(),
        "changed_deadlines":jobs.iter().zip(&aligned).filter(|(a,b)| deadline(a)!=deadline(b)).count(),
        "groups":groups.len(),"multi_job_groups":groups.iter().filter(|g|g.len()>1).count(),
        "largest_group":groups.iter().map(Vec::len).max().unwrap_or(0),"groups_split_to_preserve_daily_counts":split_groups,
        "minimum_deadline_hours":jobs.iter().map(deadline).fold(f64::INFINITY,f64::min),
        "maximum_deadline_hours":jobs.iter().map(deadline).fold(0.0,f64::max),
        "before":{"overlap":overlap_metrics(jobs,&edges),"daily":before},
        "after":{"overlap":overlap_metrics(&aligned,&edges),"daily":after},
        "invariants":["Only first_deadline_hours changes. Cadences, AOIs, priorities, collection spans and solar windows remain unchanged.",
            "Exact deadline multiset preserved separately for every cadence; therefore deadline counts in every time interval and future recurrence are preserved.",
            "Available area and priority reward can change per day because different jobs become due. Compare these denominators as well as met percentage and true reward."],
        "limitations":["Overlap grouping uses same-cadence jobs with compatible solar windows; different cadences cannot generally stay synchronized forever.",
            "Overlaps use the existing equal-area polygon model. Bounded groups and capacity-preserving splits are heuristics, not a global optimum.",
            "Day zero is relative to the graph epoch. Seven-day jobs create seven daily bands; actual longer periods use their own daily buckets."]});
    Ok(Alignment {
        jobs: aligned,
        assignments,
        report,
    })
}

// Match existing clock-time slots to the end of each cohort's local solar window.
// This avoids systematically giving the largest groups the earliest deadlines
// merely because the daily load allocator placed them first. No solar gate changes.
fn order_within_days(source: &[Job], groups: &[Vec<usize>], aligned: &mut [Job]) {
    use crate::workload::{Fulfilment, Illumination, MosaicTiming};
    let mut buckets = BTreeMap::<(u64, usize), BTreeMap<usize, Vec<usize>>>::new();
    for (g, members) in groups.iter().enumerate() {
        for &i in members {
            buckets
                .entry((
                    source[i].cadence_hours.to_bits(),
                    day(deadline(&aligned[i])),
                ))
                .or_default()
                .entry(g)
                .or_default()
                .push(i);
        }
    }
    for cohorts in buckets.values() {
        let mut times: Vec<_> = cohorts
            .values()
            .flatten()
            .map(|&i| deadline(&aligned[i]))
            .collect();
        times.sort_by(f64::total_cmp);
        let target = |ids: &Vec<usize>| {
            let mut x = 0.0;
            let mut y = 0.0;
            for &i in ids {
                let j = &source[i];
                let hour = if j.acquisition.illumination == Illumination::AnyTime {
                    deadline(j).rem_euclid(24.0)
                } else {
                    let tolerance = if j.acquisition.fulfilment == Fulfilment::Mosaic
                        && j.acquisition.mosaic_timing == MosaicTiming::CloseInTime
                    {
                        6.0
                    } else {
                        j.acquisition.solar_tolerance_hours
                    };
                    (j.acquisition.solar_time_hours + tolerance - j.aoi.longitude / 15.0)
                        .rem_euclid(24.0)
                };
                let angle = hour * std::f64::consts::TAU / 24.0;
                x += angle.cos();
                y += angle.sin();
            }
            y.atan2(x).rem_euclid(std::f64::consts::TAU) * 24.0 / std::f64::consts::TAU
        };
        let mut order: Vec<_> = cohorts.values().collect();
        order.sort_by(|a, b| {
            target(a)
                .total_cmp(&target(b))
                .then_with(|| source[a[0]].aoi.key().cmp(&source[b[0]].aoi.key()))
        });
        for (&i, time) in order.into_iter().flatten().zip(times) {
            aligned[i].acquisition.first_deadline_hours = Some(time);
        }
    }
}

fn balance(
    jobs: &[Job],
    areas: &[f64],
    groups: &[Vec<usize>],
    group_ids: &[usize],
    deadlines: &[f64],
    policy: &Policy,
    aligned: &mut [Job],
) -> usize {
    let mut slots = BTreeMap::<usize, Vec<f64>>::new();
    for &d in deadlines {
        slots.entry(day(d)).or_default().push(d);
    }
    // Pop earliest timestamps first so each group receives adjacent times within its day.
    for values in slots.values_mut() {
        values.reverse();
    }
    let capacities: BTreeMap<_, _> = slots.iter().map(|(&d, s)| (d, s.len())).collect();
    let mut global = BTreeMap::<usize, f64>::new();
    let mut local = BTreeMap::<((i32, i32), usize), f64>::new();
    let mut local_total = BTreeMap::<(i32, i32), f64>::new();
    let mut original_daily = BTreeMap::<usize, f64>::new();
    let mut total = 0.0;
    for &g in group_ids {
        for &i in &groups[g] {
            let w = weight(&jobs[i], areas[i], policy.priority_base);
            total += w;
            *original_daily.entry(day(deadline(&jobs[i]))).or_default() += w;
            *local_total
                .entry(cell(&jobs[i], policy.balance_cell_deg))
                .or_default() += w;
        }
    }
    let mut order = group_ids.to_vec();
    let group_weight = |g: usize| {
        groups[g]
            .iter()
            .map(|&i| weight(&jobs[i], areas[i], policy.priority_base))
            .sum::<f64>()
    };
    order.sort_by(|&a, &b| {
        group_weight(b)
            .total_cmp(&group_weight(a))
            .then_with(|| {
                tie(jobs, groups[a][0], policy.seed).cmp(&tie(jobs, groups[b][0], policy.seed))
            })
            .then(a.cmp(&b))
    });
    let mut split = 0;
    for g in order {
        let mut remaining = groups[g].as_slice();
        let mut used_days = 0;
        while !remaining.is_empty() {
            let max_free = slots.values().map(Vec::len).max().unwrap();
            let count = remaining.len().min(max_free);
            let part = &remaining[..count];
            let mut weights = BTreeMap::<(i32, i32), f64>::new();
            for &i in part {
                *weights
                    .entry(cell(&jobs[i], policy.balance_cell_deg))
                    .or_default() += weight(&jobs[i], areas[i], policy.priority_base);
            }
            let w = weights.values().sum::<f64>();
            let score = |d: usize| {
                let share = capacities[&d] as f64 / deadlines.len() as f64;
                let target = match policy.daily_load_target {
                    DailyLoadTarget::PerJob => total * share,
                    DailyLoadTarget::OriginalDemand => original_daily[&d],
                };
                let global_cost =
                    (global.get(&d).copied().unwrap_or(0.0) + 0.5 * w) / target.max(1e-12);
                let local_cost = weights
                    .iter()
                    .map(|(c, &cw)| {
                        cw / w.max(1e-12) * (local.get(&(*c, d)).copied().unwrap_or(0.0) + 0.5 * cw)
                            / (local_total[c] * share).max(1e-12)
                    })
                    .sum::<f64>();
                global_cost + policy.local_balance_weight * local_cost
            };
            let selected = *slots
                .iter()
                .filter(|(_, s)| s.len() >= count)
                .map(|(d, _)| d)
                .min_by(|&&a, &&b| {
                    score(a).total_cmp(&score(b)).then_with(|| {
                        (hash(format!("{}:{a}", jobs[part[0]].aoi.key()).as_bytes()) ^ policy.seed)
                            .cmp(
                                &(hash(format!("{}:{b}", jobs[part[0]].aoi.key()).as_bytes())
                                    ^ policy.seed),
                            )
                    })
                })
                .unwrap();
            for &i in part {
                aligned[i].acquisition.first_deadline_hours =
                    slots.get_mut(&selected).unwrap().pop();
            }
            *global.entry(selected).or_default() += w;
            for (c, cw) in weights {
                *local.entry((c, selected)).or_default() += cw;
            }
            remaining = &remaining[count..];
            used_days += 1;
        }
        split += usize::from(used_days > 1);
    }
    split
}
