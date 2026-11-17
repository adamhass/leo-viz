use super::super::solver::{
    self,
    experiment::{Experiment, WorkloadVariant},
    Optimizer,
};
use super::*;
use crate::workload::{AoiRef, Generator, Illumination, RewardMode};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

fn records(directory: &Path) -> Vec<serde_json::Value> {
    BufReader::new(flate2::read::GzDecoder::new(
        fs::File::open(directory.join("satellite-000.actions.jsonl.gz")).unwrap(),
    ))
    .lines()
    .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
    .collect()
}
fn identity(v: &serde_json::Value, modes: &[Mode]) -> (u64, u64, u64, u64) {
    let m = &modes[v["mode"].as_u64().unwrap() as usize];
    (
        v["start_s"].as_f64().unwrap().to_bits(),
        v["end_s"].as_f64().unwrap().to_bits(),
        m.along_km.to_bits(),
        m.cross_km.to_bits(),
    )
}
#[test]
fn full_strips_share_jobs_refinement_retains_base_and_replay_preserves_intervals() {
    let root = std::env::temp_dir().join(format!(
        "leo-full-strips-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let epoch = 1794528000.0;
    let elements = leo_sim::worldview::Fleet::WorldView60
        .shell()
        .elements(0, 0);
    let geographic = |time, cross| {
        let (q, n) = super::super::sample(&elements, epoch, time);
        let p = Mode::new(0.0, cross).offset(q, n).0;
        [(-p[2]).atan2(p[0]).to_degrees(), p[1].asin().to_degrees()]
    };
    let polygons: Vec<_> = [(60.0, 65.0, -2.0, 2.0), (64.0, 69.0, 1.0, 5.0)]
        .into_iter()
        .map(|(a, b, c, d)| {
            vec![vec![vec![
                geographic(a, c),
                geographic(b, c),
                geographic(b, d),
                geographic(a, d),
                geographic(a, c),
            ]]]
        })
        .collect();
    let refs: Vec<_> = polygons
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let anchor = p[0][0][0];
            AoiRef {
                source: "gdp".into(),
                id: format!("strip-{i}"),
                longitude: anchor[0],
                latitude: anchor[1],
                area_km2: 100.0,
                geometry_hash: crate::workload::hash(&serde_json::to_vec(p).unwrap()),
            }
        })
        .collect();
    let mut generator = Generator::default();
    generator.bounds.cadence_min_hours = 1.0 / 60.0;
    let mut jobs = generator.generate(refs.clone()).unwrap();
    for (i, j) in jobs.iter_mut().enumerate() {
        j.cadence_hours = 120.0 / 3600.0;
        j.samples = 1;
        j.pipeline = crate::workload::Pipeline::Map;
        j.acquisition.illumination = Illumination::AnyTime;
        j.acquisition.first_deadline_hours = Some((120.0 - i as f64 * 10.0) / 3600.0);
        j.acquisition.priority = 2;
        j.acquisition.reward_mode = Some(RewardMode::CompleteObservation);
    }
    let aois:Vec<_>=refs.iter().zip(&polygons).map(|(a,p)|serde_json::json!({"id":a.id,"source_kind":a.source,"area_km2":a.area_km2,"anchor_lon":a.longitude,"anchor_lat":a.latitude,"geometry":{"type":"MultiPolygon","coordinates":p}})).collect();
    let document = serde_json::json!({"schema_version":1,"preset_version":1,"config":{"count":2},"aois":aois,"workload":crate::storage::Document{schema_version:6,generator,capture:Default::default(),jobs}});
    let preset = root.join("preset.json");
    fs::write(&preset, serde_json::to_vec(&document).unwrap()).unwrap();
    let mut config = Config {
        preset,
        output: root.join("base"),
        epoch_unix_s: epoch,
        options: super::super::Options {
            duration_s: 120.0,
            reach_km: 80.0,
            along_offsets_km: vec![0.0],
            ..Default::default()
        },
        workers: 1,
        first_satellite: 0,
        satellite_count: 1,
        only_actions_serving_due_deadlines: true,
        strips: Some(Options::default()),
    };
    let base = super::super::generate(&config).unwrap();
    assert!(base.total_actions > 0);
    let original = records(&config.output);
    assert!(original
        .iter()
        .any(|a| a["end_s"].as_f64().unwrap() - a["start_s"].as_f64().unwrap() > 4.0));
    assert!(original.iter().any(|a| a["crops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["job"].as_u64().unwrap())
        .collect::<BTreeSet<_>>()
        .len()
        == 2));
    config.output = root.join("refined");
    config.strips.as_mut().unwrap().clustering = Some(Clustering::default());
    let refined = super::super::generate(&config).unwrap();
    assert!(
        refined.satellites[0]
            .strip_generation
            .as_ref()
            .unwrap()
            .refinement_actions
            > 0
    );
    let extra = records(&config.output);
    let index: BTreeMap<_, _> = extra
        .iter()
        .map(|a| (identity(a, refined.modes_for(0)), a))
        .collect();
    for a in original {
        let retained = index[&identity(&a, base.modes_for(0))];
        assert_eq!(a["crops"], retained["crops"]);
    }
    let comparison = solver::experiment::run(&Experiment {
        output: root.join("comparison"),
        workloads: vec![
            WorkloadVariant {
                id: "base".into(),
                timing_policy: "unchanged".into(),
                graph_directory: root.join("base"),
                incumbent_from: None,
            },
            WorkloadVariant {
                id: "refined".into(),
                timing_policy: "unchanged".into(),
                graph_directory: root.join("refined"),
                incumbent_from: Some("base".into()),
            },
        ],
        reward_models: [("c2".into(), Default::default())].into(),
        optimizers: vec![
            Optimizer::IndependentPaths,
            Optimizer::CoordinateImprovement,
            Optimizer::Lagrangian,
        ],
        solver: solver::Config {
            horizon_s: 120.0,
            iterations: 3,
            workers: 1,
            ..Default::default()
        },
    })
    .unwrap();
    let rows = comparison["results"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    for i in 0..3 {
        assert!(
            rows[i + 3]["true_reward"].as_f64().unwrap()
                >= rows[i]["true_reward"].as_f64().unwrap()
        );
        assert_eq!(rows[i + 3]["met_deadlines"], 2);
    }
    assert!(rows[4]["true_reward"].as_f64().unwrap() > rows[1]["true_reward"].as_f64().unwrap());
    let schedule = root.join("comparison/refined/c2/coordinate_improvement/schedule.json");
    let playback = root.join("playback.json.gz");
    super::super::playback::export(&schedule, &playback).unwrap();
    let replay = super::super::playback::Replay::load(&playback).unwrap();
    assert_eq!(replay.document.schema_version, 2);
    let requests = &replay.document.assessment.as_ref().unwrap().requests;
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(request.complete);
        let time = request
            .completed_at_s
            .expect("New exports include completion timestamps");
        assert!(time > 0.0 && time <= request.deadline_s);
        assert!(replay
            .document
            .satellites
            .iter()
            .flat_map(|s| &s.captures)
            .flat_map(|c| &c.crops)
            .any(|c| c.job == request.job
                && c.slot == request.slot
                && (c.end_s - time).abs() < 1e-6));
    }
    let capture = &replay.document.satellites[0].captures[0];
    assert!(capture.end_s - capture.start_s > 8.0);
    assert_eq!(
        replay
            .pointing(
                &replay.document.satellites[0],
                (capture.start_s + capture.end_s) / 2.0
            )
            .state,
        super::super::playback::State::Capturing
    );
    fs::remove_dir_all(root).unwrap();
}
