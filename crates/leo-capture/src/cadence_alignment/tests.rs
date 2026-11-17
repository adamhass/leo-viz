use super::*;
use crate::workload::{Acquisition, AoiRef, Generator, Illumination, Pipeline};
use std::fs;

fn fixture() -> (Vec<Job>, Vec<Region>) {
    let mut regions = Vec::new();
    let refs = (0..28)
        .map(|i| {
            let longitude = -14.0 + (i / 2) as f64 + 0.01 * (i % 2) as f64;
            let polygon = vec![vec![vec![
                [longitude, 10.0],
                [longitude + 0.04, 10.0],
                [longitude + 0.04, 10.04],
                [longitude, 10.04],
                [longitude, 10.0],
            ]]];
            let a = AoiRef {
                source: "gdp".into(),
                id: format!("alignment-{i:03}"),
                area_km2: 19.0,
                longitude,
                latitude: 10.02,
                geometry_hash: hash(&serde_json::to_vec(&polygon).unwrap()),
            };
            regions.push(Region {
                job_key: a.key(),
                polygons: polygon,
            });
            a
        })
        .collect::<Vec<_>>();
    let mut jobs = Generator::default().generate(refs).unwrap();
    for (i, j) in jobs.iter_mut().enumerate() {
        j.cadence_hours = 168.0;
        j.pipeline = Pipeline::Map;
        j.samples = 1;
        j.acquisition = Acquisition {
            first_deadline_hours: Some(5.0 + (i % 7) as f64 * 24.0 + (i / 7) as f64),
            illumination: Illumination::AnyTime,
            ..Default::default()
        };
    }
    (jobs, regions)
}

#[test]
fn weekly_bands_and_overlap_groups_preserve_demand_and_all_other_job_fields() {
    let (jobs, regions) = fixture();
    for method in [Method::LongitudeBands, Method::OverlapBalanced] {
        let policy = Policy {
            method,
            daily_load_target: DailyLoadTarget::PerJob,
            within_day_order: WithinDayOrder::Construction,
            local_balance_weight: 1.0,
            ..Default::default()
        };
        let aligned = align(&jobs, &regions, &policy).unwrap();
        export::verify_permutation(&jobs, &aligned.jobs).unwrap();
        let metric = &aligned.report["after"]["overlap"];
        assert_eq!(metric["eligible_overlap_pairs"], 14);
        assert_eq!(metric["same_day_pairs"], 14);
        assert_eq!(aligned.report["before"]["overlap"]["same_day_pairs"], 0);
        assert_eq!(aligned.report["minimum_deadline_hours"], 5.0);
        let mut counts = BTreeMap::<usize, usize>::new();
        for j in &aligned.jobs {
            *counts.entry(day(deadline(j))).or_default() += 1;
        }
        assert_eq!(counts.len(), 7);
        assert!(counts.values().all(|&n| n == 4));
        let mut reordered = jobs.clone();
        reordered.reverse();
        let result = align(&reordered, &regions, &policy).unwrap();
        let by_key = |jobs: &[Job]| {
            jobs.iter()
                .map(|j| (j.aoi.key(), deadline(j).to_bits()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(by_key(&result.jobs), by_key(&aligned.jobs));
        if method == Method::LongitudeBands {
            assert!(aligned
                .jobs
                .windows(2)
                .all(|w| deadline(&w[0]) <= deadline(&w[1])));
        }
    }
}

#[test]
fn overlap_uses_polygons_holes_cadence_and_solar_compatibility() {
    let (mut jobs, mut regions) = fixture();
    jobs.truncate(4);
    regions.truncate(4);
    // One AOI lies entirely in another AOI's hole: bounding boxes alone must not group it.
    regions[0].polygons = vec![vec![
        vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.], [0., 0.]],
        vec![[0.1, 0.1], [0.1, 0.9], [0.9, 0.9], [0.9, 0.1], [0.1, 0.1]],
    ]];
    regions[1].polygons = vec![vec![vec![
        [0.2, 0.2],
        [0.8, 0.2],
        [0.8, 0.8],
        [0.2, 0.8],
        [0.2, 0.2],
    ]]];
    regions[2].polygons = regions[1].polygons.clone();
    regions[3].polygons = regions[1].polygons.clone();
    for (j, r) in jobs.iter_mut().zip(&regions) {
        j.aoi.longitude = 0.5;
        j.aoi.latitude = 0.5;
        j.aoi.geometry_hash = hash(&serde_json::to_vec(&r.polygons).unwrap());
    }
    jobs[2].cadence_hours = 720.0;
    jobs[1].acquisition.illumination = Illumination::Daylight;
    jobs[1].acquisition.solar_time_hours = 8.0;
    jobs[1].acquisition.solar_tolerance_hours = 0.5;
    jobs[3].acquisition.illumination = Illumination::Daylight;
    jobs[3].acquisition.solar_time_hours = 16.0;
    jobs[3].acquisition.solar_tolerance_hours = 0.5;
    let policy = Policy::default();
    let atlas = Atlas::new(&jobs, &regions, 2.0, 2.0, |_| true).unwrap();
    assert!(overlap::edges(&jobs, &atlas, &policy).unwrap().is_empty());
    jobs[3].acquisition.solar_time_hours = 8.0;
    let edges = overlap::edges(&jobs, &atlas, &policy).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].a, edges[0].b), (1, 3));
}

#[test]
fn monthly_deadlines_midnight_boundaries_and_group_splits_keep_exact_counts() {
    let (mut jobs, mut regions) = fixture();
    jobs.truncate(4);
    regions.truncate(4);
    for i in 0..4 {
        jobs[i].cadence_hours = 720.0;
        jobs[i].acquisition.first_deadline_hours = Some([5., 24., 48., 720.][i]);
        regions[i].polygons = regions[0].polygons.clone();
        jobs[i].aoi.longitude = jobs[0].aoi.longitude;
        jobs[i].aoi.geometry_hash = jobs[0].aoi.geometry_hash;
    }
    let result = align(&jobs, &regions, &Policy::default()).unwrap();
    export::verify_permutation(&jobs, &result.jobs).unwrap();
    assert_eq!(result.report["multi_job_groups"], 1);
    assert_eq!(result.report["groups_split_to_preserve_daily_counts"], 1);
    let daily = &result.report["after"]["daily"];
    assert_eq!(daily[0]["due_deadlines"], 2);
    assert_eq!(daily[1]["due_deadlines"], 1);
    assert_eq!(daily[29]["due_deadlines"], 1);
}

#[test]
fn export_is_optional_roundtrips_and_refuses_to_overwrite() {
    let (jobs, regions) = fixture();
    let root = std::env::temp_dir().join(format!(
        "leo-alignment-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let aois: Vec<_> = jobs
        .iter()
        .zip(&regions)
        .map(|(j, r)| {
            serde_json::json!({
        "id":j.aoi.id,"source_kind":j.aoi.source,"area_km2":j.aoi.area_km2,
        "anchor_lon":j.aoi.longitude,"anchor_lat":j.aoi.latitude,
        "geometry":{"type":"MultiPolygon","coordinates":r.polygons}})
        })
        .collect();
    let envelope = serde_json::json!({"schema_version":1,"preset_version":1,"aois":aois,
        "config":{"count":jobs.len()},"deadline_assignment":{"seed":42},
        "workload":crate::storage::Document {schema_version:6,generator:Default::default(),capture:Default::default(),jobs:jobs.clone()}});
    let source = serde_json::to_vec(&envelope).unwrap();
    let input = root.join("preset.json");
    fs::write(&input, &source).unwrap();
    let config = export::Config {
        preset: input.clone(),
        output: root.join("aligned"),
        policy: Default::default(),
    };
    let report = export::generate(&config).unwrap();
    assert_eq!(report["status"], "complete");
    assert_eq!(fs::read(&input).unwrap(), source);
    assert!(!config.output.join("INCOMPLETE").exists());
    assert!(config.output.join("assignments.json.gz").exists());
    assert!(export::generate(&config).is_err());
    let (roundtrip, _, _) =
        crate::scheduling::load_preset(&config.output.join("preset.json.gz")).unwrap();
    export::verify_permutation(&jobs, &roundtrip).unwrap();
    let mut corrupt = roundtrip;
    corrupt[0].acquisition.priority += 1;
    assert!(export::verify_permutation(&jobs, &corrupt).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn original_demand_target_reduces_reward_moved_between_days() {
    let (mut jobs, mut regions) = fixture();
    jobs.truncate(4);
    regions.truncate(4);
    for (i, j) in jobs.iter_mut().enumerate() {
        j.acquisition.priority = if i < 2 { 1 } else { 4 };
        j.acquisition.first_deadline_hours = Some([5., 6., 29., 30.][i]);
    }
    let mut policy = Policy {
        local_balance_weight: 0.0,
        max_group_span_km: 1.0,
        daily_load_target: DailyLoadTarget::PerJob,
        ..Default::default()
    };
    let even = align(&jobs, &regions, &policy).unwrap();
    policy.daily_load_target = DailyLoadTarget::OriginalDemand;
    let matched = align(&jobs, &regions, &policy).unwrap();
    let error = |r: &Alignment| {
        r.report["before"]["daily"]
            .as_array()
            .unwrap()
            .iter()
            .zip(r.report["after"]["daily"].as_array().unwrap())
            .map(|(a, b)| {
                (a["available_priority_reward"].as_f64().unwrap()
                    - b["available_priority_reward"].as_f64().unwrap())
                .abs()
            })
            .sum::<f64>()
    };
    assert!(error(&matched) < error(&even) * 0.01);
    export::verify_permutation(&jobs, &matched.jobs).unwrap();
}

#[test]
fn solar_order_preserves_days_and_groups_and_does_not_change_solar_rules() {
    let (mut jobs, regions) = fixture();
    for (i, j) in jobs.iter_mut().enumerate() {
        j.acquisition.illumination = Illumination::Daylight;
        j.acquisition.solar_tolerance_hours = 1.0;
        j.acquisition.solar_time_hours = 8.0 + (i / 2 % 4) as f64 * 2.0;
    }
    let base = align(
        &jobs,
        &regions,
        &Policy {
            within_day_order: WithinDayOrder::Construction,
            ..Default::default()
        },
    )
    .unwrap();
    let solar = align(
        &jobs,
        &regions,
        &Policy {
            within_day_order: WithinDayOrder::SolarWindow,
            ..Default::default()
        },
    )
    .unwrap();
    export::verify_permutation(&jobs, &solar.jobs).unwrap();
    for (a, b) in base.jobs.iter().zip(&solar.jobs) {
        assert_eq!(day(deadline(a)), day(deadline(b)));
    }
    assert_eq!(
        base.report["after"]["overlap"]["same_day_pairs"],
        solar.report["after"]["overlap"]["same_day_pairs"]
    );
    for a in &solar.jobs {
        for b in &solar.jobs {
            if day(deadline(a)) == day(deadline(b))
                && a.acquisition.solar_time_hours < b.acquisition.solar_time_hours
            {
                assert!(deadline(a) < deadline(b));
            }
        }
    }
}
