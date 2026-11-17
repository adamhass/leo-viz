//! Bounded coverage-aware search for coordinate replacement paths.
//!
//! Every retained path carries its own coverage bitset. A capture earns only the
//! prices of witnesses that path has not acquired yet; a fully repeated capture
//! is skipped. Witness IDs already distinguish jobs and acquisition deadlines.
//! Partly overlapping strips remain useful when they contribute new coverage.
//!
//! Keeping every distinct coverage history can be exponential, even for one
//! satellite. We therefore retain at most K histories at each node, ranked by
//! unique reward and then capture count, and merge identical coverage states.
//! This is a proposal heuristic, NOT the additive oracle used for dual bounds
//! and multiplier updates. Exact polygon acceptance still judges the proposal.
//!
//! For A actions, E arcs and W local witnesses, propagation performs O(K² E)
//! label comparisons, plus coverage tests and O(K A ceil(W/64)) bitset copying.
//! Equal fingerprints require a bitset comparison to rule out hash collisions.
//! Bitsets are shared across wait/maneuver arcs; processed frontiers are freed.
use super::Graph;
use std::rc::Rc;

/// Compact indices are prepared once per satellite and reused across sweeps.
pub(super) struct Index {
    points: Vec<u32>,
    coverage: Vec<Vec<usize>>,
}

#[derive(Clone)]
struct Coverage {
    bits: Vec<u64>,
    fingerprint: u64,
}

#[derive(Clone)]
struct Label {
    value: f64,
    captures: usize,
    coverage: Rc<Coverage>,
    trace: Option<usize>,
}

struct Step {
    action: u32,
    previous: Option<usize>,
}

impl Index {
    pub fn new(graph: &Graph) -> Self {
        let mut points: Vec<_> = graph.coverage.iter().flatten().copied().collect();
        points.sort_unstable();
        points.dedup();
        let coverage = graph
            .coverage
            .iter()
            .map(|covered| {
                let mut local: Vec<_> = covered
                    .iter()
                    .map(|p| points.binary_search(p).unwrap())
                    .collect();
                local.sort_unstable();
                local.dedup();
                local
            })
            .collect();
        Self { points, coverage }
    }

    pub fn search(&self, graph: &Graph, prices: &[f64], limit: usize) -> (f64, Vec<u32>) {
        assert!(limit > 0);
        let weights: Vec<_> = self.points.iter().map(|&p| prices[p as usize]).collect();
        debug_assert!(weights.iter().all(|w| w.is_finite() && *w >= 0.0));
        if weights.iter().all(|w| *w == 0.0) {
            return (0.0, Vec::new());
        }
        let mut frontiers = vec![Vec::new(); graph.edges.len()];
        frontiers[graph.source].push(Label {
            value: 0.0,
            captures: 0,
            coverage: Rc::new(Coverage {
                bits: vec![0; self.points.len().div_ceil(64)],
                fingerprint: 0,
            }),
            trace: None,
        });
        let mut trace = Vec::new();
        for &from in &graph.order {
            if from == graph.sink {
                continue;
            }
            let incoming = std::mem::take(&mut frontiers[from]);
            let action = (from < 2 * graph.actions.len() && from % 2 == 0).then_some(from / 2);
            for label in incoming {
                let taken = action.and_then(|a| {
                    let gain: f64 = self.coverage[a]
                        .iter()
                        .filter(|&&p| label.coverage.bits[p / 64] & (1 << (p % 64)) == 0)
                        .map(|&p| weights[p])
                        .sum();
                    if gain <= 0.0 {
                        return None;
                    }
                    let mut coverage = (*label.coverage).clone();
                    for &p in &self.coverage[a] {
                        let bit = 1 << (p % 64);
                        if weights[p] > 0.0 && coverage.bits[p / 64] & bit == 0 {
                            coverage.bits[p / 64] |= bit;
                            coverage.fingerprint ^= fingerprint(p);
                        }
                    }
                    let id = trace.len();
                    trace.push(Step {
                        action: a as u32,
                        previous: label.trace,
                    });
                    Some(Label {
                        value: label.value + gain,
                        captures: label.captures + 1,
                        coverage: Rc::new(coverage),
                        trace: Some(id),
                    })
                });
                for &to in &graph.edges[from] {
                    // Keep skip and capture choices separate. Traversing a
                    // pointing/waiting node never implies an exposure.
                    insert(&mut frontiers[to as usize], label.clone(), limit);
                    if to as usize == from + 1 {
                        if let Some(taken) = &taken {
                            insert(&mut frontiers[to as usize], taken.clone(), limit);
                        }
                    }
                }
            }
        }
        let Some(best) = frontiers[graph.sink].first() else {
            return (0.0, Vec::new());
        };
        let mut path = Vec::new();
        let mut previous = best.trace;
        while let Some(id) = previous {
            path.push(trace[id].action);
            previous = trace[id].previous;
        }
        path.reverse();
        // A later, larger strip can make an earlier strip redundant. Remove
        // such captures one at a time without losing any positively priced
        // witness. The caller must still verify the resulting polygon reward.
        self.prune(&mut path, &weights);
        let value = weights
            .iter()
            .enumerate()
            .filter(|(p, _)| best.coverage.bits[p / 64] & (1 << (p % 64)) != 0)
            .map(|(_, weight)| weight)
            .sum();
        (value, path)
    }

    fn prune(&self, path: &mut Vec<u32>, weights: &[f64]) {
        let mut counts = vec![0usize; self.points.len()];
        for &a in path.iter() {
            for &p in &self.coverage[a as usize] {
                counts[p] += 1;
            }
        }
        let mut keep = vec![true; path.len()];
        for (i, &a) in path.iter().enumerate().rev() {
            let points = &self.coverage[a as usize];
            if points.iter().all(|&p| weights[p] == 0.0 || counts[p] > 1) {
                keep[i] = false;
                for &p in points {
                    counts[p] -= 1;
                }
            }
        }
        let mut i = 0;
        path.retain(|_| {
            i += 1;
            keep[i - 1]
        });
    }
}

/// A commutative hash screens equality checks; bitsets resolve all collisions.
fn fingerprint(point: usize) -> u64 {
    let mut value = (point as u64).wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn insert(frontier: &mut Vec<Label>, candidate: Label, limit: usize) {
    if let Some(i) = frontier.iter().position(|old| {
        Rc::ptr_eq(&old.coverage, &candidate.coverage)
            || (old.coverage.fingerprint == candidate.coverage.fingerprint
                && old.coverage.bits == candidate.coverage.bits)
    }) {
        // Identical sets have identical mathematical reward, even if their
        // accumulation orders differ slightly in floating-point arithmetic.
        if candidate.captures >= frontier[i].captures {
            return;
        }
        frontier.remove(i);
    }
    let at = frontier
        .iter()
        .position(|old| {
            candidate.value > old.value
                || (candidate.value == old.value && candidate.captures < old.captures)
        })
        .unwrap_or(frontier.len());
    if at < limit {
        frontier.insert(at, candidate);
        frontier.truncate(limit);
    }
}

#[cfg(test)]
mod tests;
