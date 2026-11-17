//! Optional optimizer-independent polygon pruning and missed-deadline refill.
mod coverage;
mod gaps;
#[cfg(test)]
mod tests;

use super::*;
use std::{cmp::Ordering, collections::BinaryHeap};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    /// Maximum accepted insertions; the report records if this limit stops refill.
    pub max_insertions: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_insertions: 10_000,
        }
    }
}
impl Options {
    pub(super) fn validate(&self) -> Result<()> {
        if !(1..=1_000_000).contains(&self.max_insertions) {
            return Err("Postprocessing max_insertions must be in 1..=1000000".into());
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Offer {
    value: f64,
    satellite: usize,
    action: u32,
}
impl PartialEq for Offer {
    fn eq(&self, b: &Self) -> bool {
        self.cmp(b).is_eq()
    }
}
impl Eq for Offer {}
impl PartialOrd for Offer {
    fn partial_cmp(&self, b: &Self) -> Option<Ordering> {
        Some(self.cmp(b))
    }
}
impl Ord for Offer {
    fn cmp(&self, b: &Self) -> Ordering {
        self.value
            .total_cmp(&b.value)
            .then_with(|| b.satellite.cmp(&self.satellite))
            .then_with(|| b.action.cmp(&self.action))
    }
}

fn prune(model: &Model, plan: &mut Plan, ledger: &mut coverage::Ledger<'_>) -> Result<usize> {
    let protected: Vec<_> = ledger
        .requests
        .iter()
        .map(|r| (r.area, r.complete, r.reward))
        .collect();
    let mut removed = 0;
    // Prefer removing later repeats, keeping earlier completion and leaving
    // later observation windows available for missed requests.
    let mut order: Vec<_> = plan
        .iter()
        .enumerate()
        .flat_map(|(s, p)| p.iter().map(move |&a| (s, a)))
        .collect();
    order.sort_by(|&(s, a), &(t, b)| {
        model.graphs[t].actions[b as usize]
            .end_s
            .total_cmp(&model.graphs[s].actions[a as usize].end_s)
            .then_with(|| t.cmp(&s))
            .then_with(|| b.cmp(&a))
    });
    let mut deleted: Vec<_> = model
        .graphs
        .iter()
        .map(|g| vec![false; g.actions.len()])
        .collect();
    let total = order.len();
    for (i, (s, a)) in order.into_iter().enumerate() {
        let change = ledger.change(s, a, false)?;
        let redundant = change.iter().all(|(r, next)| {
            next.area + ledger.tolerance(*r) >= protected[*r].0
                && (!protected[*r].1 || next.complete)
                && next.reward + 1e-8 * protected[*r].2.abs().max(1.0) >= protected[*r].2
        });
        if redundant {
            ledger.accept(change);
            removed += 1;
            deleted[s][a as usize] = true;
        }
        if i % 128 == 0 {
            resources::check_memory()?;
        }
        if (i + 1) % 500 == 0 {
            eprintln!(
                "Postprocess pruning: checked {}/{total}, removed {removed}",
                i + 1
            );
        }
    }
    for (s, path) in plan.iter_mut().enumerate() {
        path.retain(|a| !deleted[s][*a as usize]);
    }
    Ok(removed)
}

/// Enrich a feasible plan without changing its jobs, graph, or optimizer state.
///
/// 1. Assess exact per-request polygon coverage and freeze the initial missed set.
/// 2. Remove globally redundant captures one at a time, preserving all unique
///    coverage (including unrewarded partial mosaics) and fulfilled deadlines.
/// 3. Use graph reachability between retained anchors, including waiting hubs and
///    source/sink gaps, to find insertions serving those initially missed requests.
///    Lazily rank candidates by exact NEW area × priority weight. After each
///    insertion, refresh the affected satellite's gaps and the global coverage.
///    Zero immediate reward is allowed: several strips/satellites may be needed
///    to finish a completion-only mosaic. Already fulfilled requests stop driving
///    insertions; incidental benefits to other requests are still recorded.
/// 4. Prune again to remove captures made redundant by the inserted strips.
///
/// This is a greedy, bounded postprocessor, not a global optimum or a new dual
/// oracle. Its output must receive independent path/maneuver/polygon verification.
pub(super) fn run(
    model: &Model,
    geometry: &exact::Geometry,
    reward: &reward::RewardModel,
    original: &Plan,
    options: &Options,
) -> Result<(Plan, serde_json::Value)> {
    options.validate()?;
    let started = Instant::now();
    let mut plan = original.clone();
    let mut ledger = coverage::Ledger::new(model, geometry, reward, &plan)?;
    let targets: Vec<_> = ledger.requests.iter().map(|r| !r.complete).collect();
    let target_requests: Vec<_> = model.requests.iter().enumerate().filter(|(r, _)| targets[*r])
        .map(|(_, r)| serde_json::json!({"job":r.job,"slot":(r.start-model.demands[r.job].start)/model.demands[r.job].points_per_slot}))
        .collect();
    let pruned_before = prune(model, &mut plan, &mut ledger)?;
    eprintln!(
        "Postprocess: pruned {pruned_before} captures; {} missed deadlines targeted",
        target_requests.len()
    );
    let mut gaps: Vec<_> = model
        .graphs
        .iter()
        .zip(&plan)
        .map(|(g, p)| gaps::Gaps::new(g, p))
        .collect();
    let mut queue = BinaryHeap::new();
    for (s, graph) in model.graphs.iter().enumerate() {
        for a in 0..graph.actions.len() {
            if gaps[s].positions[a].is_none() {
                continue;
            }
            let value = ledger.upper(s, a, &targets);
            if !value.is_finite() {
                return Err("Non-finite postprocessing coverage value".into());
            }
            if value > 0.0 {
                queue.push(Offer {
                    value,
                    satellite: s,
                    action: a as u32,
                });
            }
        }
    }
    let initial_candidates = queue.len();
    let mut inserted = 0;
    let mut reconsidered = 0;
    while inserted < options.max_insertions {
        let Some(mut offer) = queue.pop() else {
            break;
        };
        let s = offer.satellite;
        let Some(position) = gaps[s].positions[offer.action as usize] else {
            continue;
        };
        offer.value = ledger.marginal(s, offer.action as usize, &targets)?;
        reconsidered += 1;
        if reconsidered % 128 == 0 {
            resources::check_memory()?;
        }
        if offer.value <= 0.0 {
            continue;
        }
        // Union coverage only grows during refill, so previous marginal scores
        // remain upper bounds. Requeue until this offer beats the next bound.
        if queue.peek().is_some_and(|next| offer.cmp(next).is_lt()) {
            queue.push(offer);
            continue;
        }
        let change = ledger.change(s, offer.action, true)?;
        ledger.accept(change);
        plan[s].insert(position, offer.action);
        gaps[s] = gaps::Gaps::new(&model.graphs[s], &plan[s]);
        inserted += 1;
        if inserted % 50 == 0 {
            eprintln!("Postprocess: inserted {inserted} captures");
        }
    }
    let limit_reached = inserted == options.max_insertions && !queue.is_empty();
    let pruned_after = prune(model, &mut plan, &mut ledger)?;
    for (graph, path) in model.graphs.iter().zip(&plan) {
        verify::validate_path(graph, path)?;
    }
    let elapsed_s = started.elapsed().as_secs_f64();
    eprintln!(
        "Postprocess: {inserted} inserted, {pruned_after} further removals in {elapsed_s:.2}s"
    );
    let report = serde_json::json!({"method":"prune_and_refill_missed","options":options,
        "target_requests":target_requests,"pruned_before_refill":pruned_before,"inserted_captures":inserted,
        "pruned_after_refill":pruned_after,"initial_feasible_candidates":initial_candidates,
        "marginal_evaluations":reconsidered,"polygon_request_evaluations":ledger.evaluations,
        "stop_reason":if limit_reached {"insertion_limit"} else {"no_remaining_feasible_gain"},
        "elapsed_s":elapsed_s,"cached_true_reward":ledger.total_reward(),
        "refill_objective":"New polygon area for initially missed requests multiplied by priority_base^priority; partial mosaics may earn coverage before they earn true reward.",
        "pruning_relative_area_tolerance":1e-10});
    Ok((plan, report))
}
