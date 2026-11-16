//! Replay controls and planned deadline metrics.
use super::*;

impl Panel {
    pub fn ui(&mut self, ui: &mut egui::Ui, tab: &mut TabConfig) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            ui.label("Saved schedule replay");
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .hint_text("playback.json.gz")
                    .desired_width(f32::INFINITY),
            );
            if ui
                .add_enabled(self.pending.is_none(), egui::Button::new("Load schedule"))
                .clicked()
            {
                self.start(ui.ctx());
            }
            if self.pending.is_some() {
                ui.add(
                    egui::ProgressBar::new(self.progress.0)
                        .show_percentage()
                        .animate(true),
                );
                ui.label(&self.progress.1);
                if let Some(started) = self.started {
                    ui.small(format!(
                        "Loading · {:.1}s elapsed",
                        started.elapsed().as_secs_f32()
                    ));
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        #[cfg(target_arch = "wasm32")]
        ui.label("Saved schedule loading is available in the desktop app.");
        if let Some(e) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, e);
        }
        let Some(scene) = tab.observation_replay.clone() else {
            ui.small(
                "Loads the saved AOIs, jobs, constellation and selected capture plan together.",
            );
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        if ui
            .add_enabled(self.pending.is_none(), egui::Button::new("Leave replay"))
            .clicked()
        {
            tab.settings.animate = false;
            self.leave(ui.ctx(), scene.clone());
        }
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut tab.settings.pushbroom.show_observable,
                "Show observation reach",
            );
            ui.checkbox(&mut tab.settings.pushbroom.show_fan, "Show sensor fan");
        });
        let replay = &scene.replay;
        let horizon = replay.document.horizon_s;
        ui.horizontal(|ui| {
            if ui
                .button(if tab.settings.animate {
                    "Pause schedule"
                } else {
                    "Play schedule"
                })
                .clicked()
            {
                if tab.settings.time >= horizon {
                    tab.settings.time = 0.0;
                }
                tab.settings.animate = !tab.settings.animate;
            }
            if ui.button("Restart").clicked() {
                tab.settings.time = 0.0;
                tab.settings.animate = false;
            }
            if ui.button("Next capture").clicked() {
                if let Some(t) = replay.next_capture(tab.settings.time) {
                    // Show the sweep itself, not the zero-area start boundary.
                    tab.settings.time = t + replay.document.options.step_s * 0.5;
                    if let Some(satellite) = replay
                        .document
                        .satellites
                        .iter()
                        .find(|s| s.captures.iter().any(|c| c.start_s == t))
                    {
                        let p = replay.pointing(satellite, tab.settings.time).centre;
                        let lat = p[1].clamp(-1.0, 1.0).asin();
                        let lon = -p[2].atan2(p[0]);
                        tab.settings.rotation = crate::math::lat_lon_to_matrix(lat, lon);
                        tab.settings.earth_fixed_camera = true;
                        tab.settings.sun_fixed_camera = false;
                        tab.settings.camera_roll = 0.0;
                        tab.settings.camera_mode = crate::config::CameraMode::Unlocked;
                        tab.settings.zoom = tab.settings.zoom.clamp(3.0, 20.0);
                        tab.settings.auto_zoom = false;
                        tab.planets[0].location_search.camera = None;
                    }
                    tab.settings.animate = false;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Playback speed");
            ui.add(
                egui::DragValue::new(&mut tab.settings.speed)
                    .range(0.1..=3600.0)
                    .suffix("×"),
            );
            for speed in [1.0, 60.0, 600.0] {
                if ui.small_button(format!("{speed:.0}×")).clicked() {
                    tab.settings.speed = speed;
                }
            }
        });
        let mut hours = tab.settings.time / 3600.0;
        if ui
            .add(
                egui::Slider::new(&mut hours, 0.0..=horizon / 3600.0)
                    .text("hours")
                    .clamping(egui::SliderClamping::Edits)
                    // Format the label only. fixed_decimals rounds the bound clock
                    // on every frame and can erase all advances below 1.8 seconds.
                    .custom_formatter(|hours, _| format!("{hours:.3}")),
            )
            .changed()
        {
            tab.settings.time = hours * 3600.0;
        }
        let instant =
            scene.epoch() + chrono::Duration::milliseconds((tab.settings.time * 1000.0) as i64);
        ui.label(instant.format("%Y-%m-%d %H:%M:%S UTC").to_string());
        let completed = replay.captured_segments(tab.settings.time);
        let touched = replay
            .first_capture_end_s
            .iter()
            .filter(|&&t| t <= tab.settings.time)
            .count();
        let scheduled = replay.scheduled_jobs.iter().filter(|&&a| a).count();
        let mut states = [0usize; 3];
        for satellite in &replay.document.satellites {
            let i = match replay.pointing(satellite, tab.settings.time).state {
                State::Capturing => 0,
                State::Slewing | State::Settling => 1,
                State::Idle => 2,
            };
            states[i] += 1;
        }
        let [active, moving, idle] = states;
        ui.label(format!(
            "{completed} / {} capture segments · {active} capturing · {moving} repointing/settling · {idle} idle",
            replay.capture_ends.len()
        ));
        ui.label(format!(
            "{touched} / {scheduled} planned AOIs touched · {} inactive AOIs",
            replay
                .scheduled_jobs
                .iter()
                .zip(&replay.missed_jobs)
                .filter(|(selected, missed)| !**selected && !**missed)
                .count()
        ));
        if let Some(assessment) = &replay.document.assessment {
            let due = assessment.requests.len();
            let missed = assessment.requests.iter().filter(|r| !r.complete).count();
            let rate = if due == 0 {
                0.0
            } else {
                100.0 * missed as f64 / due as f64
            };
            let met = due - missed;
            let met_rate = if due == 0 {
                0.0
            } else {
                100.0 * met as f64 / due as f64
            };
            ui.strong(format!("{met} / {due} due deadlines met ({met_rate:.2}%)"));
            ui.colored_label(
                egui::Color32::from_rgb(MISSED_COLOR[0], MISSED_COLOR[1], MISSED_COLOR[2]),
                format!("{missed} / {due} due deadlines missed by this plan ({rate:.1}%)"),
            );
            ui.small("Red: missed deadline in this plan, including partial coverage. Later deadlines are not failures.");
        } else {
            ui.colored_label(egui::Color32::YELLOW, "This older replay has no per-job deadline results. Re-export it to show missed deadlines in red.");
        }
        ui.colored_label(
            egui::Color32::from_rgb(ACTIVE_COLOR[0], ACTIVE_COLOR[1], ACTIVE_COLOR[2]),
            "Teal: selected coverage · gray: inactive",
        );
        ui.colored_label(
            egui::Color32::LIGHT_GREEN,
            "Green: capture sweep and recent 15-minute trail",
        );
        ui.colored_label(
            crate::pushbroom::OBSERVABLE_COLOR,
            "Dashed cyan: observation reach (available access)",
        );
        ui.small("Sensor fan: amber while repointing/settling · gray while idle · green while capturing.");
        ui.small("One-second sweeps follow the saved plan. Trails persist across playback steps and fade with simulation time. Red is the final planning result, not an execution failure; jobs continue after missed deadlines.");
    }
}
