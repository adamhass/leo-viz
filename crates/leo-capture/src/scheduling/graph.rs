use super::{Mode, Options};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Action {
    /// Sampling index, retained for file compatibility; timing uses seconds.
    pub tick: u32,
    pub mode: usize,
    pub start_s: f64,
    pub end_s: f64,
}
impl Action {
    pub fn at_tick(tick: u32, mode: usize, step_s: f64) -> Self {
        Self {
            tick,
            mode,
            start_s: f64::from(tick) * step_s,
            end_s: f64::from(tick + 1) * step_s,
        }
    }
}

/// Exact transition feasibility for the finite candidate set and configured
/// quaternion slew law. Durations are never rounded to an imaging time step.
/// Nonoverlapping candidates in each mode form a wait/skip spine; only the
/// earliest reachable action in each other mode needs a direct transition.
pub struct Topology {
    pub actions: Vec<Action>,
    pub next_same: Vec<Option<u32>>,
    by_mode: Vec<Vec<u32>>,
    slew_s: Vec<Vec<f64>>,
    initial_slew_s: Vec<f64>,
}
impl Topology {
    pub fn new(actions: Vec<Action>, modes: &[Mode], options: &Options) -> Result<Self, String> {
        if actions.len() > (u32::MAX as usize - 2) / 2 {
            return Err("Too many graph nodes".into());
        }
        let mut by_mode = vec![Vec::new(); modes.len()];
        for (i, a) in actions.iter().enumerate() {
            if a.mode >= modes.len()
                || !a.start_s.is_finite()
                || !a.end_s.is_finite()
                || a.start_s < 0.0
                || a.end_s <= a.start_s
                || a.end_s > options.duration_s + 1e-9
            {
                return Err("Invalid action interval".into());
            }
            let ids = &mut by_mode[a.mode];
            if ids
                .last()
                .is_some_and(|&p| actions[p as usize].end_s > a.start_s + 1e-9)
            {
                return Err("Actions must be ordered and nonoverlapping within each mode".into());
            }
            ids.push(i as u32);
        }
        let mut next_same = vec![None; actions.len()];
        for ids in &by_mode {
            for pair in ids.windows(2) {
                next_same[pair[0] as usize] = Some(pair[1]);
            }
        }
        let slew_s = modes
            .iter()
            .map(|a| modes.iter().map(|b| a.transition_s(b, options)).collect())
            .collect();
        let nadir = Mode::new(0.0, 0.0);
        let initial_slew_s = modes
            .iter()
            .map(|m| nadir.transition_s(m, options))
            .collect();
        Ok(Self {
            actions,
            next_same,
            by_mode,
            slew_s,
            initial_slew_s,
        })
    }
    fn first(&self, mode: usize, ready_s: f64) -> Option<u32> {
        let ids = &self.by_mode[mode];
        ids.get(ids.partition_point(|&a| self.actions[a as usize].start_s + 1e-9 < ready_s))
            .copied()
    }
    pub fn source_targets(&self) -> Vec<u32> {
        (0..self.by_mode.len())
            .filter_map(|m| self.first(m, self.initial_slew_s[m]))
            .collect()
    }
    pub fn source_slew_s(&self, target: u32) -> f64 {
        self.initial_slew_s[self.actions[target as usize].mode]
    }
    pub fn transition_s(&self, from: usize, to: u32) -> f64 {
        self.slew_s[self.actions[from].mode][self.actions[to as usize].mode]
    }
    pub fn targets(&self, id: usize) -> Vec<u32> {
        let a = self.actions[id];
        (0..self.by_mode.len())
            .filter(|&m| m != a.mode)
            .filter_map(|m| self.first(m, a.end_s + self.slew_s[a.mode][m]))
            .collect()
    }
    pub fn reachable(&self) -> Vec<bool> {
        let mut reachable = vec![false; self.actions.len()];
        for a in self.source_targets() {
            reachable[a as usize] = true;
        }
        let mut order: Vec<_> = (0..self.actions.len()).collect();
        order.sort_by(|&a, &b| {
            self.actions[a]
                .start_s
                .total_cmp(&self.actions[b].start_s)
                .then(a.cmp(&b))
        });
        for id in order {
            if !reachable[id] {
                continue;
            }
            if let Some(next) = self.next_same[id] {
                reachable[next as usize] = true;
            }
            for next in self.targets(id) {
                reachable[next as usize] = true;
            }
        }
        reachable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn paths(t: &Topology, sparse: bool) -> BTreeSet<Vec<u32>> {
        fn visit(
            t: &Topology,
            id: usize,
            sparse: bool,
            taken: &mut Vec<u32>,
            out: &mut BTreeSet<Vec<u32>>,
        ) {
            for capture in [false, true] {
                if capture {
                    taken.push(id as u32);
                }
                out.insert(taken.clone());
                let a = t.actions[id];
                let next: Vec<_> = if sparse {
                    t.next_same[id].into_iter().chain(t.targets(id)).collect()
                } else {
                    t.actions
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| b.start_s + 1e-9 >= a.end_s + t.slew_s[a.mode][b.mode])
                        .map(|(i, _)| i as u32)
                        .collect()
                };
                for n in next {
                    visit(t, n as usize, sparse, taken, out);
                }
                if capture {
                    taken.pop();
                }
            }
        }
        let mut out = BTreeSet::from([Vec::new()]);
        let starts = if sparse {
            t.source_targets()
        } else {
            t.actions
                .iter()
                .enumerate()
                .filter(|(_, a)| a.start_s + 1e-9 >= t.initial_slew_s[a.mode])
                .map(|(i, _)| i as u32)
                .collect()
        };
        for i in starts {
            visit(t, i as usize, sparse, &mut Vec::new(), &mut out);
        }
        out
    }
    #[test]
    fn sparse_wait_spines_preserve_every_dense_capture_sequence() {
        let o = Options {
            duration_s: 180.0,
            step_s: 30.0,
            ..Default::default()
        };
        let modes = vec![Mode::new(0.0, 0.0), Mode::new(0.0, 100.0)];
        let actions = vec![
            Action::at_tick(0, 0, 30.0),
            Action::at_tick(1, 0, 30.0),
            Action::at_tick(1, 1, 30.0),
            Action::at_tick(2, 1, 30.0),
            Action::at_tick(4, 0, 30.0),
            Action::at_tick(5, 1, 30.0),
        ];
        let t = Topology::new(actions, &modes, &o).unwrap();
        assert_eq!(paths(&t, true), paths(&t, false));
        assert!(paths(&t, true).contains(&vec![0, 4])); // B need not be acquired.
        assert!(!paths(&t, true).contains(&vec![0, 2])); // Repointing is not free.
        assert!(t.reachable().iter().all(|x| *x));
    }
    #[test]
    fn initial_pointing_and_long_idle_gaps() {
        let o = Options {
            duration_s: 600.0,
            step_s: 30.0,
            ..Default::default()
        };
        let modes = vec![Mode::new(0.0, 0.0), Mode::new(0.0, 200.0)];
        let t = Topology::new(
            vec![Action::at_tick(0, 1, 30.0), Action::at_tick(10, 1, 30.0)],
            &modes,
            &o,
        )
        .unwrap();
        assert_eq!(t.reachable(), vec![false, true]);
        assert_eq!(t.source_targets(), vec![1]);
        assert_eq!(paths(&t, true), paths(&t, false));
    }
    #[test]
    fn fractional_slews_admit_small_adjustments_without_a_thirty_second_gap() {
        let o = Options {
            duration_s: 100.0,
            ..Default::default()
        };
        let modes = [
            Mode::new(0.0, 0.0),
            Mode::new(0.0, 11.79),
            Mode::new(0.0, 200.0),
        ];
        let small = modes[0].transition_s(&modes[1], &o);
        let large = modes[0].transition_s(&modes[2], &o);
        assert!(small < large && small < 3.0);
        let actions = vec![
            Action {
                tick: 0,
                mode: 0,
                start_s: 0.0,
                end_s: 0.75,
            },
            Action {
                tick: 1,
                mode: 1,
                start_s: 0.75 + small - 0.01,
                end_s: 0.75 + small - 0.005,
            },
            Action {
                tick: 2,
                mode: 1,
                start_s: 0.75 + small,
                end_s: 1.75 + small,
            },
            Action {
                tick: 3,
                mode: 2,
                start_s: 0.75 + small,
                end_s: 1.75 + small,
            },
            Action {
                tick: 4,
                mode: 2,
                start_s: 0.75 + large,
                end_s: 1.75 + large,
            },
        ];
        let t = Topology::new(actions, &modes, &o).unwrap();
        assert_eq!(t.targets(0), vec![2, 4]);
        assert!((t.transition_s(0, 2) - small).abs() < 1e-12);
        assert_eq!(paths(&t, true), paths(&t, false));
    }
}
