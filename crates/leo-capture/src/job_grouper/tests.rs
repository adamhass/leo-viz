use super::*;
use crate::{
    geometry::Region,
    workload::{hash, Acquisition, AoiRef, Fulfilment, Generator, MosaicTiming, Pipeline},
};

fn fixture(rings: Vec<Vec<Vec<[f64; 2]>>>) -> (Vec<Job>, Vec<Region>) {
    let regions: Vec<_> = rings
        .into_iter()
        .enumerate()
        .map(|(i, rings)| Region {
            job_key: format!("gdp:group-{i}"),
            polygons: vec![rings],
        })
        .collect();
    let refs = regions.iter().enumerate().map(|(i, r)| AoiRef {
        source: "gdp".into(),
        id: format!("group-{i}"),
        area_km2: 100.0,
        longitude: r.polygons[0][0][0][0],
        latitude: r.polygons[0][0][0][1],
        geometry_hash: hash(&serde_json::to_vec(&r.polygons).unwrap()),
    });
    let mut jobs = Generator::default().generate(refs).unwrap();
    for job in &mut jobs {
        job.cadence_hours = 48.0;
        job.samples = 1;
        job.pipeline = Pipeline::Map;
        job.acquisition = Acquisition {
            first_deadline_hours: Some(24.0),
            illumination: Illumination::AnyTime,
            fulfilment: Fulfilment::Mosaic,
            mosaic_timing: MosaicTiming::CloseInTime,
            mosaic_span_hours: Some(24.0),
            ..Default::default()
        };
    }
    let mut regions = regions;
    for (r, j) in regions.iter_mut().zip(&jobs) {
        r.job_key = j.aoi.key();
    }
    (jobs, regions)
}
fn rectangle(w: f64, e: f64, s: f64, n: f64) -> Vec<[f64; 2]> {
    vec![[w, s], [e, s], [e, n], [w, n], [w, s]]
}
fn atlas(jobs: &[Job], regions: &[Region]) -> Atlas {
    Atlas::with_workers(jobs, regions, 2.0, 2.0, 1, |_| true).unwrap()
}

#[test]
fn unions_preserve_jobs_holes_and_cross_chart_overlap_and_allow_different_cadences() {
    let (mut jobs, regions) = fixture(vec![
        vec![
            rectangle(-0.03, 0.03, -0.03, 0.03),
            rectangle(-0.01, 0.01, -0.01, 0.01),
        ],
        vec![rectangle(0.02, 0.05, -0.02, 0.02)],
        vec![rectangle(-0.005, 0.005, -0.005, 0.005)], // inside the first AOI's hole
    ]);
    jobs[1].cadence_hours = 72.0;
    let before = jobs.clone();
    let atlas = atlas(&jobs, &regions);
    let result = enrich(&jobs, &atlas, &Context::default(), &Policy::default()).unwrap();
    assert_eq!(result.jobs, &before);
    assert!(std::ptr::eq(result.jobs.as_ptr(), jobs.as_ptr()));
    assert_eq!(result.groups.len(), 1);
    let group = &result.groups[0];
    assert_eq!(
        group.members.iter().map(|m| m.job).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(result.report.grouped_jobs, 2);
    assert!(
        group.pieces.len() > 1,
        "Union must preserve chart boundaries"
    );
    let mut overlap = 0.0;
    for patch in &atlas.patches {
        for a in patch.pieces.iter().filter(|p| p.job == 0) {
            for b in patch.pieces.iter().filter(|p| p.job == 1) {
                overlap += a.shape.intersection(&b.shape).unsigned_area();
            }
        }
    }
    assert!(
        (group.union_area_km2 - (atlas.job_areas_km2[0] + atlas.job_areas_km2[1] - overlap)).abs()
            < 1e-6
    );
    for p in &group.pieces {
        for hole_target in atlas.patches[p.patch].pieces.iter().filter(|p| p.job == 2) {
            assert!(p.shape.intersection(&hole_target.shape).unsigned_area() < 1e-9);
        }
    }
}

#[test]
fn chain_requires_a_common_window_for_every_member() {
    let (mut jobs, regions) = fixture(vec![vec![rectangle(0.0, 0.02, 0.0, 0.02)]; 3]);
    // A:08-10, B:09-12, C:11-13. AB and BC overlap; ABC does not.
    for (job, (start, end)) in jobs
        .iter_mut()
        .zip([(8.0, 10.0), (9.0, 12.0), (11.0, 13.0)])
    {
        job.acquisition.first_deadline_hours = Some(end);
        job.acquisition.mosaic_span_hours = Some(end - start);
    }
    let result = enrich(
        &jobs,
        &atlas(&jobs, &regions),
        &Context::default(),
        &Policy::default(),
    )
    .unwrap();
    assert_eq!(result.groups.len(), 1);
    assert_eq!(result.groups[0].members.len(), 2);
    assert_eq!(
        result.groups[0].windows,
        vec![Window {
            start_s: 9.0 * 3600.0,
            end_s: 10.0 * 3600.0
        }]
    );
    assert!(result.report.rejected_common_window > 0);
}

#[test]
fn solar_windows_are_evaluated_at_each_original_anchor() {
    let (mut jobs, regions) = fixture(vec![vec![rectangle(0.0, 0.02, 0.0, 0.02)]; 2]);
    for (i, job) in jobs.iter_mut().enumerate() {
        job.acquisition.illumination = Illumination::Daylight;
        job.acquisition.mosaic_timing = MosaicTiming::SimilarSolarTime;
        job.acquisition.solar_time_hours = if i == 0 { 8.0 } else { 16.0 };
        job.acquisition.solar_tolerance_hours = 0.5;
    }
    let atlas = atlas(&jobs, &regions);
    assert!(
        enrich(&jobs, &atlas, &Context::default(), &Policy::default())
            .unwrap()
            .groups
            .is_empty()
    );
    jobs[1].acquisition.solar_time_hours = 8.0;
    let result = enrich(&jobs, &atlas, &Context::default(), &Policy::default()).unwrap();
    assert_eq!(result.groups.len(), 1);
    let frames = solar_frames(&Context::default(), &Policy::default());
    for window in &result.groups[0].windows {
        let t = (window.start_s + window.end_s) / 2.0;
        let f = &frames[(t / 60.0).floor() as usize][1];
        let local = f.greenwich_hours + jobs[0].aoi.longitude / 15.0;
        assert!((local - 8.0).abs() <= 0.5);
    }
    // Close-in-time mosaics have a daylight gate but no preferred solar-hour gate.
    jobs[1].acquisition.mosaic_timing = MosaicTiming::CloseInTime;
    jobs[1].acquisition.solar_time_hours = 16.0;
    assert_eq!(
        enrich(&jobs, &atlas, &Context::default(), &Policy::default())
            .unwrap()
            .groups
            .len(),
        1
    );
}

#[test]
fn size_limit_tracks_swaths_window_lengths_and_contiguous_capture_time() {
    let c = Context::default();
    let p = Policy::default();
    let eight = [Window {
        start_s: 0.0,
        end_s: 8.0 * 3600.0,
    }];
    let four = [Window {
        start_s: 0.0,
        end_s: 4.0 * 3600.0,
    }];
    let fit = size_constraint(36.0, 30.0, &eight, &c, &p);
    assert!(fit.accepted);
    assert_eq!(fit.strips, 3);
    assert_eq!(fit.strip_budget, 3);
    assert!(!size_constraint(37.0, 30.0, &eight, &c, &p).accepted);
    assert!(!size_constraint(36.0, 30.0, &four, &c, &p).accepted);
    assert!(
        size_constraint(
            10.0,
            30.0,
            &[Window {
                start_s: 0.0,
                end_s: 60.0
            }],
            &c,
            &p
        )
        .accepted
    );
    assert!(
        !size_constraint(
            10.0,
            30.0,
            &[Window {
                start_s: 0.0,
                end_s: 10.0
            }],
            &c,
            &p
        )
        .accepted
    );
    assert!(!size_constraint(10.0, 200.0, &eight, &c, &p).accepted);
    assert!(!size_constraint(f64::NAN, 30.0, &eight, &c, &p).accepted);
}

#[test]
fn repeated_job_cycles_stay_separate_and_future_scope_is_explicit() {
    let (mut jobs, regions) = fixture(vec![vec![rectangle(0.0, 0.02, 0.0, 0.02)]; 2]);
    for job in &mut jobs {
        job.cadence_hours = 4.0;
        job.acquisition.first_deadline_hours = Some(2.0);
        job.acquisition.mosaic_span_hours = Some(4.0);
    }
    let atlas = atlas(&jobs, &regions);
    let result = enrich(&jobs, &atlas, &Context::default(), &Policy::default()).unwrap();
    assert_eq!(result.groups.len(), 6);
    for g in &result.groups {
        assert_eq!(g.members.len(), 2);
        assert_eq!(g.members[0].slot, g.members[1].slot);
    }
    for j in &mut jobs {
        j.cadence_hours = 48.0;
        j.acquisition.first_deadline_hours = Some(30.0);
        j.acquisition.mosaic_span_hours = Some(24.0);
    }
    assert!(
        enrich(&jobs, &atlas, &Context::default(), &Policy::default())
            .unwrap()
            .groups
            .is_empty()
    );
    let all = Context {
        due_only: false,
        ..Default::default()
    };
    let result = enrich(&jobs, &atlas, &all, &Policy::default()).unwrap();
    assert_eq!(result.groups.len(), 1);
    assert_eq!(
        result.groups[0].windows,
        vec![Window {
            start_s: 6.0 * 3600.0,
            end_s: 24.0 * 3600.0
        }]
    );
}

#[test]
fn membership_caps_and_input_order_are_deterministic() {
    let (jobs, regions) = fixture(vec![vec![rectangle(0.0, 0.02, 0.0, 0.02)]; 5]);
    let policy = Policy {
        max_members: 3,
        ..Default::default()
    };
    let a = enrich(&jobs, &atlas(&jobs, &regions), &Context::default(), &policy).unwrap();
    assert_eq!(a.report.members_per_group.min, Some(2.0));
    assert_eq!(a.report.members_per_group.max, Some(3.0));
    assert_eq!(a.report.members_per_group.mean, Some(2.5));
    assert_eq!(a.report.members_per_group.median, Some(2.5));
    let mut reversed = jobs.clone();
    reversed.reverse();
    let b = enrich(
        &reversed,
        &atlas(&reversed, &regions),
        &Context::default(),
        &policy,
    )
    .unwrap();
    let names = |e: &EnrichedJobs| {
        e.groups
            .iter()
            .map(|g| {
                g.members
                    .iter()
                    .map(|m| (e.jobs[m.job].aoi.key(), m.slot))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&a), names(&b));
}

#[test]
fn grouped_strip_graph_preserves_base_and_retains_strips_serving_subsets() {
    use crate::scheduling::{self, strips};
    use std::{
        fs,
        io::{BufRead, BufReader},
    };
    let context = Context {
        horizon_s: 120.0,
        ..Default::default()
    };
    let elements = leo_sim::worldview::Fleet::WorldView60
        .shell()
        .elements(0, 0);
    let geo = |time, cross| {
        let (q, n) = scheduling::sample(&elements, context.epoch_unix_s, time);
        let angle: f64 = cross / R;
        let p: Vector = std::array::from_fn(|i| q[i] * angle.cos() + n[i] * angle.sin());
        [(-p[2]).atan2(p[0]).to_degrees(), p[1].asin().to_degrees()]
    };
    let rings = [(-2.0, 9.0), (8.0, 20.0), (19.0, 30.0)]
        .into_iter()
        .map(|(a, b)| {
            vec![vec![
                geo(60.0, a),
                geo(65.0, a),
                geo(65.0, b),
                geo(60.0, b),
                geo(60.0, a),
            ]]
        })
        .collect();
    let (mut jobs, regions) = fixture(rings);
    for j in &mut jobs {
        j.acquisition.first_deadline_hours = Some(120.0 / 3600.0);
    }
    let root = std::env::temp_dir().join(format!(
        "leo-grouped-strips-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let aois:Vec<_>=jobs.iter().zip(&regions).map(|(j,r)|serde_json::json!({"id":j.aoi.id,"source_kind":j.aoi.source,"area_km2":j.aoi.area_km2,"anchor_lon":j.aoi.longitude,"anchor_lat":j.aoi.latitude,"geometry":{"type":"MultiPolygon","coordinates":r.polygons}})).collect();
    let preset = root.join("preset.json");
    fs::write(&preset,serde_json::to_vec(&serde_json::json!({"schema_version":1,"preset_version":1,"config":{"count":3},"aois":aois,"workload":crate::storage::Document{schema_version:6,generator:Generator::default(),capture:Default::default(),jobs}})).unwrap()).unwrap();
    let mut config = scheduling::Config {
        preset,
        output: root.join("base"),
        epoch_unix_s: context.epoch_unix_s,
        options: scheduling::Options {
            duration_s: 120.0,
            reach_km: 100.0,
            along_offsets_km: vec![0.0],
            ..Default::default()
        },
        workers: 1,
        first_satellite: 0,
        satellite_count: 1,
        only_actions_serving_due_deadlines: true,
        strips: Some(strips::Options::default()),
    };
    let read = |dir: &std::path::Path| -> Vec<serde_json::Value> {
        BufReader::new(flate2::read::GzDecoder::new(
            fs::File::open(dir.join("satellite-000.actions.jsonl.gz")).unwrap(),
        ))
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
    };
    let base = scheduling::generate(&config).unwrap();
    let original = read(&config.output);
    config.output = root.join("grouped");
    config.strips.as_mut().unwrap().grouping = Some(Policy {
        nominal_access_gap_hours: 20.0 / 3600.0,
        ..Default::default()
    });
    let grouped = scheduling::generate(&config).unwrap();
    let actions = read(&config.output);
    assert_eq!(grouped.job_count, 3);
    assert!(
        grouped.satellites[0]
            .strip_generation
            .as_ref()
            .unwrap()
            .group_actions
            > 0
    );
    assert!(actions.iter().any(|a| a["origin"] == "group"
        && a["seed_jobs"].as_array().unwrap().len() == 3
        && a["crops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["job"].as_u64().unwrap())
            .collect::<BTreeSet<_>>()
            .len()
            < 3));
    let identity = |a: &serde_json::Value, modes: &[scheduling::Mode]| {
        let mode = &modes[a["mode"].as_u64().unwrap() as usize];
        (
            a["start_s"].as_f64().unwrap().to_bits(),
            a["end_s"].as_f64().unwrap().to_bits(),
            mode.along_km.to_bits(),
            mode.cross_km.to_bits(),
        )
    };
    for a in original {
        let candidate = actions
            .iter()
            .find(|b| identity(&a, base.modes_for(0)) == identity(b, grouped.modes_for(0)))
            .expect("Retain original candidate");
        assert_eq!(a["crops"], candidate["crops"]);
    }
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(config.output.join("grouping-report.json")).unwrap())
            .unwrap();
    assert_eq!(report["groups_added"], 1);
    fs::remove_dir_all(root).unwrap();
}
