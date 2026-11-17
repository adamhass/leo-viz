use super::loader::LoadResult;
use super::*;
pub(super) fn fixture() -> Scene {
    use leo_capture::{
        scheduling::{
            playback::{Capture, Crop, Document, Satellite},
            Options,
        },
        workload::{AoiRef, Generator},
    };
    let generator = Generator::default();
    let mut jobs = generator
        .generate((0..2).map(|i| AoiRef {
            source: "gdp".into(),
            id: i.to_string(),
            area_km2: 1.0,
            latitude: 0.0,
            longitude: 0.0,
            geometry_hash: 1,
        }))
        .unwrap();
    for j in &mut jobs {
        j.acquisition.phase_hours = 0.0;
    }
    let options = Options::default();
    let modes = options.modes();
    let mode = modes
        .iter()
        .position(|m| m.along_km == 0.0 && m.cross_km == 0.0)
        .unwrap();
    let capture = |action, t| Capture {
        action,
        tick: t as u32,
        mode,
        start_s: t,
        end_s: t + 1.0,
        crops: vec![Crop {
            job: 0,
            slot: 0,
            start_s: t,
            end_s: t + 1.0,
        }],
    };
    let replay=Replay::new(Document{assessment:None,schema_version:1,epoch_unix_s:1794528000.0,horizon_s:120.0,options,modes,
        constellation:leo_sim::worldview::Fleet::WorldView60.shell(),
        preset:serde_json::json!({"workload":leo_capture::storage::Document{schema_version:5,generator,capture:Default::default(),jobs}}),
        satellites:vec![Satellite{satellite:0,captures:vec![capture(0,100.0),capture(1,110.0)]}],
        selected_candidate:"test".into(),source_preset_hash_fnv1a64:0}).unwrap();
    let footprints = replay.document.satellites[0]
        .captures
        .iter()
        .map(|c| {
            let a = replay.scanline(0, c.mode, c.start_s);
            let b = replay.scanline(0, c.mode, c.end_s);
            Footprint {
                satellite: 0,
                mode: c.mode,
                start_s: c.start_s,
                end_s: c.end_s,
                corners: [a[0], a[1], b[1], b[0]],
            }
        })
        .collect();
    Scene {
        job_colors: status::JobColors::new(&replay),
        layer_jobs: vec![],
        color_time: std::sync::Mutex::new(None),
        replay,
        layers: vec![],
        footprints,
    }
}
#[test]
fn playback_clock_pauses_clamps_and_seeks_without_losing_events() {
    let mut tab = TabConfig::new("Replay".into());
    tab.observation_replay = Some(Arc::new(fixture()));
    tab.settings.animate = false;
    tab.settings.speed = 600.0;
    assert_eq!(advance(&mut tab, 1.0), 0.0);
    tab.settings.animate = true;
    assert_eq!(advance(&mut tab, 1.0), 120.0);
    assert!(!tab.settings.animate);
    assert_eq!(
        tab.observation_replay
            .as_ref()
            .unwrap()
            .replay
            .captured_segments(tab.settings.time),
        2
    );
    tab.settings.time = 100.5;
    assert_eq!(
        tab.observation_replay
            .as_ref()
            .unwrap()
            .replay
            .captured_segments(tab.settings.time),
        0
    );
    assert_eq!(epoch(&tab, chrono::Utc::now()).timestamp(), 1794528000);
}
#[test]
fn replay_controls_start_pause_and_skip_to_the_next_capture() {
    fn frame(
        ctx: &egui::Context,
        panel: &mut Panel,
        tab: &mut TabConfig,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.ui(ui, tab),
        )
    }
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    panel.auto_load = false;
    let mut tab = TabConfig::new("Replay".into());
    tab.observation_replay = Some(Arc::new(fixture()));
    tab.settings.animate = false;
    for label in ["Play schedule", "Pause schedule", "Next capture", "Restart"] {
        for _ in 0..2 {
            frame(&ctx, &mut panel, &mut tab, vec![]);
        }
        let out = frame(&ctx, &mut panel, &mut tab, vec![]);
        let p = out
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing {label}"));
        for pressed in [true, false] {
            frame(
                &ctx,
                &mut panel,
                &mut tab,
                vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        match label {
            "Play schedule" => assert!(tab.settings.animate),
            "Pause schedule" => assert!(!tab.settings.animate),
            "Next capture" => assert_eq!(tab.settings.time, 100.5),
            "Restart" => assert_eq!(tab.settings.time, 0.0),
            _ => {}
        }
    }
}
#[test]
fn rendering_controls_never_quantizes_or_stalls_the_running_clock() {
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    panel.auto_load = false;
    let mut tab = TabConfig::new("Replay".into());
    tab.observation_replay = Some(Arc::new(fixture()));
    for speed in [0.1, 1.0, 60.0, 600.0] {
        tab.settings.time = 0.0;
        tab.settings.speed = speed;
        tab.settings.animate = true;
        for _ in 0..60 {
            advance(&mut tab, 1.0 / 60.0);
            let before = tab.settings.time;
            let _ = ctx.run_ui(Default::default(), |ui| panel.ui(ui, &mut tab));
            assert_eq!(
                tab.settings.time, before,
                "Rendering must not edit the clock"
            );
        }
        assert!((tab.settings.time - speed.min(120.0)).abs() < 1e-8);
    }
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn loading_progress_is_visible_and_failed_load_preserves_the_scene() {
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    panel.auto_load = false;
    let mut tab = TabConfig::new("Replay".into());
    let scene = Arc::new(fixture());
    tab.observation_replay = Some(scene.clone());
    let mut aoi = crate::aoi::AoiGenerator::default();
    let (tx, rx) = std::sync::mpsc::channel();
    panel.pending = Some(rx);
    tx.send(LoadEvent::Progress(
        0.6,
        "Preparing saved geometry · 8 CPU workers".into(),
    ))
    .unwrap();
    panel.poll(&ctx, &mut aoi, &mut tab);
    assert_eq!(panel.progress.0, 0.6);
    let out = ctx.run_ui(Default::default(), |ui| panel.ui(ui, &mut tab));
    assert!(out.shapes.iter().any(
        |s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text.contains("8 CPU workers"))
    ));
    tx.send(LoadEvent::Finished(Err("invalid replay".into())))
        .unwrap();
    panel.poll(&ctx, &mut aoi, &mut tab);
    assert!(panel.pending.is_none());
    assert_eq!(panel.error.as_deref(), Some("invalid replay"));
    assert!(Arc::ptr_eq(
        tab.observation_replay.as_ref().unwrap(),
        &scene
    ));
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn loading_a_schedule_enables_reach_and_sensor_fan() {
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    panel.auto_load = false;
    let mut tab = TabConfig::new("Replay".into());
    tab.settings.pushbroom.show_observable = false;
    tab.settings.pushbroom.show_fan = false;
    let mut aoi = crate::aoi::AoiGenerator::default();
    let (tx, rx) = std::sync::mpsc::channel();
    panel.pending = Some(rx);
    tx.send(LoadEvent::Finished(Ok(LoadResult::Schedule {
        scene: fixture(),
        workload: crate::aoi::storage::Loaded {
            layers: std::array::from_fn(|_| None),
            workload: None,
        },
    })))
    .unwrap();
    panel.poll(&ctx, &mut aoi, &mut tab);
    assert!(tab.settings.pushbroom.observable_visible());
    assert_eq!(tab.settings.pushbroom.observation_reach_km(), 450.0);
    assert!(tab.settings.pushbroom.show_fan);
    assert!(!tab.settings.pushbroom.show_images);
}
#[test]
#[ignore = "Loads the saved 53k-AOI replay artifact; run explicitly for integration verification"]
fn saved_worldview_replay_loads_all_aois_and_only_selected_captures() {
    let path = std::path::Path::new(DEFAULT_REPLAY_PATH);
    let mut stages = Vec::new();
    let started = std::time::Instant::now();
    let (scene, loaded) = Scene::load(path.to_str().unwrap(), |f, message| {
        stages.push((f, message));
    })
    .unwrap();
    assert!(stages.windows(2).all(|w| w[0].0 <= w[1].0));
    assert_eq!(stages.last().unwrap().0, 1.0);
    assert!(stages.iter().any(|(_, m)| m.contains("CPU workers")));
    let assessment = scene
        .replay
        .document
        .assessment
        .as_ref()
        .expect("Deadline results in current bundle");
    assert_eq!(assessment.requests.len(), 2334);
    assert_eq!(
        assessment.requests.iter().filter(|r| !r.complete).count(),
        967
    );
    assert_eq!(scene.replay.missed_jobs.iter().filter(|&&m| m).count(), 967);
    eprintln!(
        "Loaded complete replay in {:.2}s",
        started.elapsed().as_secs_f32()
    );
    assert_eq!(scene.replay.jobs.len(), 53000);
    assert_eq!(scene.layers.iter().map(|p| p.count).sum::<usize>(), 53000);
    assert_eq!(scene.replay.capture_ends.len(), 22462);
    assert_eq!(scene.replay.document.satellites.len(), 60);
    assert!(scene.replay.scheduled_jobs.iter().filter(|&&s| s).count() > 1613);
    assert_eq!(
        scene.replay.document.selected_candidate,
        "independent_paths_baseline"
    );
    assert_eq!(scene.replay.captured_segments(86400.0), 22462);
    assert_eq!(scene.replay.captured_segments(0.0), 0);
    assert_eq!(loaded.workload.unwrap().jobs, scene.replay.jobs);
    let mut tab = TabConfig::new("Replay".into());
    tab.observation_replay = Some(Arc::new(scene));
    tab.settings.time = 86400.0;
    let ctx = egui::Context::default();
    let mut panel = Panel::default();
    let _ = ctx.run_ui(Default::default(), |ui| panel.ui(ui, &mut tab));
}
