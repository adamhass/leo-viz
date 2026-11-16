//! Native background loading. Publish a scene only after every stage succeeds.
use super::*;

pub(super) enum LoadEvent {
    Progress(f32, String),
    Finished(Result<LoadResult, String>),
}

/// Loading and leaving replay publish distinct, fully prepared results.
pub(super) enum LoadResult {
    Schedule {
        scene: Scene,
        workload: crate::aoi::storage::Loaded,
    },
    Workload(crate::aoi::storage::Loaded),
}

impl Panel {
    pub(super) fn start(&mut self, ctx: &egui::Context) {
        let path = self.path.clone();
        self.spawn_load(ctx, "Opening schedule…", move |progress| {
            Scene::load(&path, progress)
                .map(|(scene, workload)| LoadResult::Schedule { scene, workload })
        });
    }

    pub(super) fn leave(&mut self, ctx: &egui::Context, scene: Arc<Scene>) {
        self.spawn_load(ctx, "Restoring normal AOI colors…", move |progress| {
            crate::aoi::storage::load_embedded(&scene.replay.document.preset, None, progress)
                .map(LoadResult::Workload)
        });
    }

    fn spawn_load(
        &mut self,
        ctx: &egui::Context,
        message: &str,
        load: impl FnOnce(&mut dyn FnMut(f32, String)) -> Result<LoadResult, String> + Send + 'static,
    ) {
        if self.pending.is_some() {
            return;
        }
        self.error = None;
        self.progress = (0.0, message.into());
        self.started = Some(std::time::Instant::now());
        let ctx = ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            leo_capture::resources::background_thread();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                load(&mut |fraction, message| {
                    let _ = tx.send(LoadEvent::Progress(fraction, message));
                    ctx.request_repaint();
                })
            }))
            .unwrap_or_else(|_| Err("Replay loader stopped; current scene was kept".into()));
            let _ = tx.send(LoadEvent::Finished(result));
            ctx.request_repaint();
        });
    }
    pub fn poll(
        &mut self,
        ctx: &egui::Context,
        aoi: &mut crate::aoi::AoiGenerator,
        tab: &mut TabConfig,
    ) {
        if self.auto_load {
            self.auto_load = false;
            self.start(ctx);
        }
        let mut result = None;
        if let Some(rx) = &self.pending {
            loop {
                match rx.try_recv() {
                    Ok(LoadEvent::Progress(f, message)) => {
                        self.progress = (self.progress.0.max(f), message)
                    }
                    Ok(LoadEvent::Finished(r)) => {
                        result = Some(r);
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(_) => {
                        result = Some(Err(
                            "Replay loader disconnected; current scene was kept".into()
                        ));
                        break;
                    }
                }
            }
        }
        if let Some(result) = result {
            self.pending = None;
            match result {
                Err(e) => self.error = Some(e),
                Ok(LoadResult::Workload(loaded)) => {
                    aoi.install_replay_workload(loaded);
                    tab.observation_replay = None;
                    tab.settings.animate = false;
                    tab.settings.time = 0.0;
                }
                Ok(LoadResult::Schedule {
                    scene,
                    workload: loaded,
                }) => {
                    aoi.install_replay_workload(loaded);
                    let mut c = crate::config::ConstellationConfig::new(0);
                    crate::orbit_presets::apply_worldview(
                        &mut c,
                        leo_sim::worldview::Fleet::WorldView60,
                    );
                    let planet = &mut tab.planets[0];
                    planet.constellations = vec![c];
                    for (selected, _, _) in planet.tle_selections.values_mut() {
                        *selected = false;
                    }
                    planet.pushbroom_history.clear();
                    planet.satellite_cameras.clear();
                    planet.pending_cameras.clear();
                    tab.settings.time = 0.0;
                    tab.settings.animate = false;
                    tab.settings.speed = 60.0;
                    tab.settings.pushbroom.apply_worldview();
                    tab.settings.pushbroom.show_observable = true;
                    tab.settings.pushbroom.show_fan = true;
                    tab.settings.pushbroom.show_images = false;
                    tab.settings.auto_rotate = false;
                    tab.settings.show_links = false;
                    tab.settings.show_coverage = false;
                    tab.observation_replay = Some(Arc::new(scene));
                    ctx.request_repaint();
                }
            }
        }
    }
}
