//! Exact interval transitions, including overlapping alternatives in one mode.
//! Nearby transitions use the actual slew. After a conservative maximum slew,
//! a shared chronological wait chain reaches every later action without O(n²) arcs.
use super::{export, Action, Config, Mode};
use std::time::Instant;

pub(super) struct Topology {
    pub source: Vec<u32>,
    pub direct: Vec<Vec<u32>>,
    pub wait: Vec<Option<usize>>,
    pub reachable: Vec<bool>,
}
impl Topology {
    pub fn new(
        actions: &[Action],
        modes: &[Mode],
        options: &super::Options,
    ) -> Result<Self, String> {
        let nadir = Mode::new(0.0, 0.0);
        let max_rotation = modes
            .iter()
            .map(|m| (nadir.transition_s(m, options) - options.settle_s).max(0.0))
            .fold(0.0, f64::max);
        let max_slew = 2.0 * max_rotation + options.settle_s + 1e-8;
        let mut out = Self {
            source: Vec::new(),
            direct: vec![Vec::new(); actions.len()],
            wait: vec![None; actions.len()],
            reachable: vec![false; actions.len()],
        };
        for (i, a) in actions.iter().enumerate() {
            if a.mode >= modes.len()
                || !a.start_s.is_finite()
                || !a.end_s.is_finite()
                || a.start_s < 0.0
                || a.end_s <= a.start_s
                || a.end_s > options.duration_s + 1e-9
                || i > 0 && a.start_s < actions[i - 1].start_s
            {
                return Err("Invalid or unordered full-strip interval".into());
            }
            if a.start_s + 1e-9 >= nadir.transition_s(&modes[a.mode], options) {
                out.source.push(i as u32);
                out.reachable[i] = true;
            }
            let first = actions.partition_point(|b| b.start_s + 1e-9 < a.end_s);
            let distant = actions.partition_point(|b| b.start_s < a.end_s + max_slew);
            for j in first..distant {
                if actions[j].start_s + 1e-9
                    >= a.end_s + modes[a.mode].transition_s(&modes[actions[j].mode], options)
                {
                    out.direct[i].push(j as u32);
                }
            }
            out.wait[i] = (distant < actions.len()).then_some(distant);
        }
        let mut earliest_wait = actions.len();
        for i in 0..actions.len() {
            out.reachable[i] |= i >= earliest_wait;
            if out.reachable[i] {
                for &j in &out.direct[i] {
                    out.reachable[j as usize] = true;
                }
                if let Some(j) = out.wait[i] {
                    earliest_wait = earliest_wait.min(j);
                }
            }
        }
        Ok(out)
    }
}

pub(super) fn write(
    config: &Config,
    satellite: usize,
    actions: &[Action],
    modes: &[Mode],
) -> Result<(u64, u64, u64, u64, usize, f64), String> {
    let started = Instant::now();
    let topology = Topology::new(actions, modes, &config.options)?;
    let base = 2 * actions.len() + 2;
    let nodes = base + actions.len();
    let target = config
        .output
        .join(format!("satellite-{satellite:03}.graph.jsonl.gz"));
    let part = target.with_extension("gz.part");
    let mut writer = export::writer(&part)?;
    export::line(
        &mut writer,
        &serde_json::json!({"schema_version":4,"satellite":satellite,"action_count":actions.len(),"nodes":nodes,"source":base-2,"sink":base-1,"source_to":topology.source,"transition_encoding":"intervals_with_wait_chain","rules":"Action start=2a, end=2a+1. Nearby arcs use exact slew times. Waiting hubs reach all actions after a conservative maximum slew; overlapping alternatives remain independent."}),
    )?;
    let mut edges = 3 * actions.len() + topology.source.len() + 1;
    let mut maneuvers = 0;
    for (i, a) in actions.iter().enumerate() {
        export::line(
            &mut writer,
            &serde_json::json!({"action":i,"tick":a.tick,"mode":a.mode,"start_s":a.start_s,"end_s":a.end_s,"next_same":null,"maneuver_to":topology.direct[i],"maneuver_hub":topology.wait[i].map(|j|base+j)}),
        )?;
        edges += topology.direct[i].len() + usize::from(topology.wait[i].is_some());
        maneuvers += topology.direct[i].len();
        export::line(
            &mut writer,
            &serde_json::json!({"hub":base+i,"targets":[i],"next_hub":(i+1<actions.len()).then_some(base+i+1)}),
        )?;
        edges += 1 + usize::from(i + 1 < actions.len());
    }
    let bytes = export::finish(writer, &part, &target)?;
    Ok((
        nodes as u64,
        edges as u64,
        maneuvers as u64,
        bytes,
        topology.reachable.iter().filter(|&&r| !r).count(),
        started.elapsed().as_secs_f64(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_interval_reachability_matches_dense_slew_constraints() {
        let options = super::super::Options {
            duration_s: 300.0,
            ..Default::default()
        };
        let modes = vec![
            Mode::new(0.0, 0.0),
            Mode::new(0.0, 1.0),
            Mode::new(150.0, -150.0),
        ];
        let spans = [
            (20., 40., 0),
            (20., 25., 0),
            (25., 29., 0),
            (27., 31., 1),
            (40., 45., 2),
            (100., 110., 1),
            (100., 120., 1),
            (121., 130., 0),
        ];
        let actions: Vec<_> = spans
            .into_iter()
            .map(|(start_s, end_s, mode)| Action {
                tick: start_s as u32,
                start_s,
                end_s,
                mode,
            })
            .collect();
        let topology = Topology::new(&actions, &modes, &options).unwrap();
        for i in 0..actions.len() {
            let mut reached = vec![false; actions.len()];
            for &j in &topology.direct[i] {
                reached[j as usize] = true;
            }
            if let Some(j) = topology.wait[i] {
                reached[j..].fill(true);
            }
            for j in 0..actions.len() {
                if reached[j] {
                    for &k in &topology.direct[j] {
                        reached[k as usize] = true;
                    }
                    if let Some(k) = topology.wait[j] {
                        reached[k..].fill(true);
                    }
                }
            }
            for (j, b) in actions.iter().enumerate() {
                assert_eq!(
                    reached[j],
                    b.start_s + 1e-9
                        >= actions[i].end_s
                            + modes[actions[i].mode].transition_s(&modes[b.mode], &options),
                    "{i} -> {j}"
                );
            }
        }
        assert!(topology.direct[1].contains(&2)); // Short alternative can precede another capture.
        assert!(!topology.direct[0].contains(&2)); // Long overlapping alternative cannot.
    }
}
