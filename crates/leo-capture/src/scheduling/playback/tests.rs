use super::*;

#[test]
fn replay_file_roundtrip_and_failed_writes_preserve_existing_outputs() {
    use super::io::{read_json, write_new_gzip};
    let dir = std::env::temp_dir().join(format!("leo-replay-io-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("replay.json.gz");
    write_new_gzip(&path, &fixture()).unwrap();
    let replay = Replay::load(&path).unwrap();
    assert_eq!(replay.capture_ends.len(), 2);
    assert!(write_new_gzip(&path, &serde_json::json!({"replacement": true})).is_err());
    assert_eq!(read_json(&path).unwrap()["schema_version"], 1);

    struct Broken;
    impl Serialize for Broken {
        fn serialize<S: serde::Serializer>(&self, _: S) -> std::result::Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("serialization failed"))
        }
    }
    let failed = dir.join("failed.json.gz");
    assert!(write_new_gzip(&failed, &Broken).is_err());
    assert!(!failed.exists());
    assert!(!dir.join("failed.json.gz.part").exists());
    assert!(!dir.join("replay.json.gz.part").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

fn fixture() -> Document {
    let generator = crate::workload::Generator::default();
    let mut jobs = generator
        .generate([crate::workload::AoiRef {
            source: "gdp".into(),
            id: "replay".into(),
            area_km2: 1.0,
            latitude: 0.0,
            longitude: 0.0,
            geometry_hash: 1,
        }])
        .unwrap();
    jobs[0].acquisition.illumination = crate::workload::Illumination::AnyTime;
    jobs[0].acquisition.first_deadline_hours = Some(1.0);
    let options = Options::default();
    let modes = options.modes();
    let a = modes
        .iter()
        .position(|m| m.along_km == 0.0 && (m.cross_km - 11.79).abs() < 1e-8)
        .unwrap();
    let b = modes
        .iter()
        .position(|m| m.along_km == 0.0 && (m.cross_km + 11.79).abs() < 1e-8)
        .unwrap();
    let capture = |action, mode, start| Capture {
        action,
        tick: start as u32,
        mode,
        start_s: start,
        end_s: start + 1.0,
        crops: vec![Crop {
            job: 0,
            slot: 0,
            start_s: start,
            end_s: start + 1.0,
        }],
    };
    Document {
        assessment: None,
        schema_version: 1,
        epoch_unix_s: 1794528000.0,
        horizon_s: 120.0,
        options,
        modes,
        constellation: leo_sim::worldview::Fleet::WorldView60.shell(),
        preset: serde_json::json!({"workload":crate::storage::Document{schema_version:5,generator,capture:Default::default(),jobs}}),
        satellites: vec![Satellite {
            satellite: 0,
            captures: vec![capture(0, a, 100.0), capture(1, b, 110.0)],
        }],
        selected_candidate: "test".into(),
        source_preset_hash_fnv1a64: 0,
    }
}
#[test]
fn missed_deadlines_include_partial_coverage_but_never_future_jobs() {
    let mut doc = fixture();
    doc.assessment = Some(Assessment {
        job_cadence_hours: None,
        requests: vec![],
    });
    let pending = Replay::new(doc).unwrap();
    assert!(pending.scheduled_jobs[0]);
    assert!(!pending.missed_jobs[0]); // 1h deadline is outside the 120s run.
    for (fraction, complete) in [(0.0, false), (0.5, false), (1.0, true)] {
        let mut doc = fixture();
        doc.horizon_s = 4000.0;
        doc.assessment = Some(Assessment {
            job_cadence_hours: None,
            requests: vec![DeadlineOutcome {
                completed_at_s: None,
                job: 0,
                slot: 0,
                deadline_s: 3600.0,
                coverage_fraction: fraction,
                complete,
            }],
        });
        let replay = Replay::new(doc).unwrap();
        assert_eq!(replay.missed_jobs[0], !complete);
    }
    let mut doc = fixture();
    doc.assessment = Some(Assessment {
        job_cadence_hours: None,
        requests: vec![DeadlineOutcome {
            completed_at_s: None,
            job: 0,
            slot: 0,
            deadline_s: 3600.0,
            coverage_fraction: 0.0,
            complete: false,
        }],
    });
    assert!(
        Replay::new(doc).is_err(),
        "A future deadline cannot be marked missed"
    );
}

#[test]
fn completion_timestamps_roundtrip_and_legacy_assessments_still_load() {
    let make = |time| {
        let mut doc = fixture();
        doc.horizon_s = 4000.0;
        doc.assessment = Some(Assessment {
            job_cadence_hours: None,
            requests: vec![DeadlineOutcome {
                job: 0,
                slot: 0,
                deadline_s: 3600.0,
                coverage_fraction: 1.0,
                complete: true,
                completed_at_s: time,
            }],
        });
        doc
    };
    for time in [None, Some(101.0), Some(111.0)] {
        let json = serde_json::to_value(make(time)).unwrap();
        assert_eq!(
            json["assessment"]["requests"][0]
                .get("completed_at_s")
                .is_some(),
            time.is_some()
        );
        let replay = Replay::new(serde_json::from_value(json).unwrap()).unwrap();
        assert_eq!(
            replay.document.assessment.unwrap().requests[0].completed_at_s,
            time
        );
    }
    for time in [
        f64::NAN,
        f64::INFINITY,
        -1.0,
        0.0,
        100.0,
        105.0,
        112.0,
        3601.0,
        4001.0,
    ] {
        assert!(
            Replay::new(make(Some(time))).is_err(),
            "Accepted invalid completion {time}"
        );
    }
    let mut incomplete = make(Some(101.0));
    let request = &mut incomplete.assessment.as_mut().unwrap().requests[0];
    request.complete = false;
    request.coverage_fraction = 0.5;
    assert!(
        Replay::new(incomplete).is_err(),
        "Partial coverage cannot have a completion timestamp"
    );
}
#[test]
fn incomplete_or_inconsistent_assessments_are_rejected() {
    let mut doc = fixture();
    doc.horizon_s = 4000.0;
    doc.assessment = Some(Assessment {
        job_cadence_hours: None,
        requests: vec![],
    });
    assert!(Replay::new(doc).is_err());
    let mut doc = fixture();
    doc.horizon_s = 4000.0;
    doc.assessment = Some(Assessment {
        job_cadence_hours: None,
        requests: vec![DeadlineOutcome {
            completed_at_s: None,
            job: 0,
            slot: 0,
            deadline_s: 3600.0,
            coverage_fraction: 0.5,
            complete: true,
        }],
    });
    assert!(Replay::new(doc).is_err());
}
#[test]
fn clock_seeking_and_maneuvers_never_capture_during_idle_slew_or_settling() {
    let replay = Replay::new(fixture()).unwrap();
    let sat = &replay.document.satellites[0];
    assert_eq!(replay.pointing(sat, 90.0).state, State::Idle);
    assert_eq!(replay.pointing(sat, 97.6).state, State::Slewing);
    assert_eq!(replay.pointing(sat, 99.0).state, State::Settling);
    assert_eq!(replay.pointing(sat, 100.0).state, State::Capturing);
    assert_eq!(replay.pointing(sat, 100.999).state, State::Capturing);
    assert_eq!(replay.pointing(sat, 101.0).state, State::Idle);
    assert_eq!(replay.pointing(sat, 109.0).state, State::Settling);
    assert_eq!(replay.pointing(sat, 110.0).state, State::Capturing);
    assert_eq!(replay.pointing(sat, 120.0).state, State::Idle);
    for (t, count) in [
        (120.0, 2),
        (0.0, 0),
        (100.5, 0),
        (101.0, 1),
        (110.5, 1),
        (111.0, 2),
    ] {
        assert_eq!(replay.captured_segments(t), count);
    }
    assert_eq!(replay.next_capture(0.0), Some(100.0));
    assert_eq!(replay.next_capture(100.0), Some(110.0));
    assert_eq!(replay.scheduled_jobs, vec![true]);
    assert_eq!(replay.first_capture_end_s, vec![101.0]);
    let p = replay.pointing(sat, 100.5);
    let edge = edges(p.centre, p.normal, replay.document.options.width_km);
    assert_eq!(edge, replay.scanline(0, sat.captures[0].mode, 100.5));
}
#[test]
fn interpolation_follows_constant_angular_rate_and_exact_endpoints() {
    let a = Mode::new(-150.0, 90.0);
    let b = Mode::new(150.0, -90.0);
    let angle = |a: &Mode, b: &Mode| {
        2.0 * a
            .attitude
            .iter()
            .zip(b.attitude)
            .map(|(x, y)| x * y)
            .sum::<f64>()
            .abs()
            .clamp(0.0, 1.0)
            .acos()
    };
    for t in [0.0, 0.1, 0.5, 0.9, 1.0] {
        let m = interpolate(&a, &b, t);
        assert!((angle(&a, &m) - t * angle(&a, &b)).abs() < 1e-7);
    }
    assert_eq!(interpolate(&a, &b, 0.0).attitude, a.attitude);
    assert_eq!(interpolate(&a, &b, 1.0).attitude, b.attitude);
}
#[test]
fn invalid_identity_geometry_timing_and_crop_windows_are_rejected() {
    let mut d = fixture();
    d.satellites[0].captures[1].start_s = 101.0;
    assert!(Replay::new(d).is_err());
    let mut d = fixture();
    d.satellites[0].captures[0].crops[0].job = 1;
    assert!(Replay::new(d).is_err());
    let mut d = fixture();
    d.satellites[0].captures[0].crops[0].end_s = 105.0;
    assert!(Replay::new(d).is_err());
    let mut d = fixture();
    d.modes[0].attitude[0] = f64::NAN;
    assert!(Replay::new(d).is_err());
    let mut d = fixture();
    d.satellites.push(d.satellites[0].clone());
    assert!(Replay::new(d).is_err());
    let json = serde_json::to_vec(&fixture()).unwrap();
    let restored = Replay::new(serde_json::from_slice(&json).unwrap()).unwrap();
    assert_eq!(restored.capture_ends, vec![101.0, 111.0]);
}
