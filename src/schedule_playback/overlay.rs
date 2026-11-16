use super::Scene;
use crate::globe_overlay::{front_polygon, front_segment, line_quad, project_surface};
use eframe::egui::{self, Color32, Mesh, Shape};
use egui_plot::{PlotBounds, PlotGeometry, PlotItem, PlotItemBase, PlotPoint, PlotTransform};
use leo_capture::scheduling::playback::{edges, State};
use leo_sim::geometry::Vector;
use nalgebra::{Matrix3, Vector3};
use std::ops::RangeInclusive;

pub(crate) struct Overlay<'a> {
    base: PlotItemBase,
    settings: crate::pushbroom::Settings,
    scene: &'a Scene,
    time: f64,
    rotation: Matrix3<f64>,
    radius: f64,
    polar_scale: f64,
    projection: Option<&'a dyn crate::projection::Projection>,
}
impl<'a> Overlay<'a> {
    pub fn new(
        scene: &'a Scene,
        time: f64,
        rotation: Matrix3<f64>,
        radius: f64,
        flattening: f64,
    ) -> Self {
        let mut settings = crate::pushbroom::Settings::default();
        settings.apply_worldview();
        settings.show_fan = true;
        Self {
            settings,
            base: PlotItemBase::new("Scheduled captures".into()),
            scene,
            time,
            rotation,
            radius,
            polar_scale: 1.0 - flattening,
            projection: None,
        }
    }
    pub fn projected(
        scene: &'a Scene,
        time: f64,
        projection: &'a dyn crate::projection::Projection,
    ) -> Self {
        let mut result = Self::new(scene, time, Matrix3::identity(), 1.0, 0.0);
        result.projection = Some(projection);
        result
    }
    pub fn with_settings(mut self, settings: crate::pushbroom::Settings) -> Self {
        self.settings = settings;
        self
    }
    fn geographic(p: Vector) -> [f64; 2] {
        [
            -p[2].atan2(p[0]).to_degrees(),
            p[1].clamp(-1.0, 1.0).asin().to_degrees(),
        ]
    }
    fn screen(transform: &PlotTransform, p: Vector3<f64>) -> egui::Pos2 {
        transform.position_from_point(&PlotPoint::new(p.x, p.y))
    }
    fn line(
        &self,
        mesh: &mut Mesh,
        transform: &PlotTransform,
        a: Vector,
        b: Vector,
        color: Color32,
        width: f32,
    ) {
        self.styled_line(mesh, transform, a, b, color, width, false);
    }
    fn styled_line(
        &self,
        mesh: &mut Mesh,
        transform: &PlotTransform,
        a: Vector,
        b: Vector,
        color: Color32,
        width: f32,
        dashed: bool,
    ) {
        let mut phase = 0.0;
        let mut draw = |mesh: &mut Mesh, a, b| {
            if dashed {
                crate::pushbroom::dashed_line(mesh, a, b, *transform.frame(), color, &mut phase);
            } else {
                line_quad(mesh, a, b, color, width);
            }
        };
        if let Some(projection) = self.projection {
            for path in crate::projection::project_path(
                projection,
                &[Self::geographic(a), Self::geographic(b)],
            ) {
                for pair in path.windows(2) {
                    let point =
                        |p: [f64; 2]| transform.position_from_point(&PlotPoint::new(p[0], p[1]));
                    draw(mesh, point(pair[0]), point(pair[1]));
                }
            }
        } else {
            let project = |p: Vector| {
                project_surface(
                    Vector3::from(p),
                    &self.rotation,
                    self.radius,
                    self.polar_scale,
                )
            };
            if let Some((a, b)) = front_segment(project(a), project(b)) {
                draw(mesh, Self::screen(transform, a), Self::screen(transform, b));
            }
        }
    }
    fn polygon(
        &self,
        mesh: &mut Mesh,
        transform: &PlotTransform,
        points: &[Vector; 4],
        color: Color32,
    ) {
        let projected: Vec<_> = if let Some(projection) = self.projection {
            let ll: Vec<_> = points.iter().map(|&p| Self::geographic(p)).collect();
            let mapped: Option<Vec<_>> =
                ll.iter().map(|p| projection.project(p[1], p[0])).collect();
            let Some(mapped) = mapped else {
                return;
            };
            let span = projection.x_range().1 - projection.x_range().0;
            // Never paint across a projection seam. Outlines below still split
            // at the seam, including dateline and interrupted polar projections.
            if mapped
                .iter()
                .any(|a| mapped.iter().any(|b| (a.0 - b.0).abs() > span / 4.0))
            {
                return;
            }
            mapped
                .into_iter()
                .map(|(x, y)| transform.position_from_point(&PlotPoint::new(x, y)))
                .collect()
        } else {
            front_polygon(&points.map(|p| {
                project_surface(
                    Vector3::from(p),
                    &self.rotation,
                    self.radius,
                    self.polar_scale,
                )
            }))
            .into_iter()
            .map(|p| Self::screen(transform, p))
            .collect()
        };
        if projected.len() < 3 {
            return;
        }
        let first = mesh.vertices.len() as u32;
        for p in projected {
            mesh.colored_vertex(p, color);
        }
        for i in first + 1..mesh.vertices.len() as u32 - 1 {
            mesh.add_triangle(first, i, i + 1);
        }
    }
    fn reach_mesh(&self, mesh: &mut Mesh, transform: &PlotTransform) {
        if !self.settings.observable_visible() {
            return;
        }
        let replay = &self.scene.replay;
        let now = self.time.clamp(0.0, replay.document.horizon_s);
        let trail = self.settings.trail_s.clamp(5.0, 300.0);
        let start = (now - trail).max(0.0);
        let steps = ((now - start) / 10.0).ceil().max(1.0) as usize;
        let width = self.settings.observation_reach_km();
        let color = crate::pushbroom::OBSERVABLE_COLOR;
        for satellite in &replay.document.satellites {
            let mut previous = replay.access_scanline(satellite.satellite, start, width);
            for step in 1..=steps {
                let time = start + (now - start) * step as f64 / steps as f64;
                let next = replay.access_scanline(satellite.satellite, time, width);
                let fade = (1.0 - (now - time) / trail) as f32;
                self.polygon(
                    mesh,
                    transform,
                    &[previous[0], previous[1], next[1], next[0]],
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 18)
                        .gamma_multiply(fade),
                );
                for side in 0..2 {
                    self.styled_line(
                        mesh,
                        transform,
                        previous[side],
                        next[side],
                        color.gamma_multiply(0.5 * fade),
                        1.0,
                        true,
                    );
                }
                previous = next;
            }
            self.styled_line(mesh, transform, previous[0], previous[1], color, 1.0, true);
        }
    }
    fn mesh(&self, transform: &PlotTransform) -> Mesh {
        let mut mesh = Mesh::default();
        let replay = &self.scene.replay;
        // Reach lies behind the narrow scheduled strips and current sensor fan.
        self.reach_mesh(&mut mesh, transform);
        for footprint in self
            .scene
            .footprints
            .iter()
            .skip_while(|f| f.end_s < self.time - super::TRAIL_SECONDS)
            .take_while(|f| f.start_s < self.time)
        {
            let corners = if footprint.end_s <= self.time {
                footprint.corners
            } else {
                let b = replay.scanline(footprint.satellite, footprint.mode, self.time);
                [footprint.corners[0], footprint.corners[1], b[1], b[0]]
            };
            let age = (self.time - footprint.end_s).max(0.0);
            let fade = (1.0 - age / super::TRAIL_SECONDS).clamp(0.0, 1.0) as f32;
            self.polygon(
                &mut mesh,
                transform,
                &corners,
                Color32::from_rgba_unmultiplied(65, 240, 120, 110).gamma_multiply(fade),
            );
            // Thin acquired strips remain visible when zoomed out.
            for i in [0, 2] {
                self.line(
                    &mut mesh,
                    transform,
                    corners[i],
                    corners[(i + 1) % 4],
                    Color32::from_rgba_unmultiplied(65, 240, 120, 230).gamma_multiply(fade),
                    2.0,
                );
            }
        }
        for satellite in &replay.document.satellites {
            let p = replay.pointing(satellite, self.time);
            if p.state != State::Capturing && !self.settings.show_fan {
                continue;
            }
            let color = match p.state {
                State::Capturing => Color32::from_rgb(100, 255, 145),
                State::Slewing | State::Settling => Color32::from_rgb(255, 185, 65),
                State::Idle => Color32::from_rgb(160, 170, 185),
            };
            let edges = edges(p.centre, p.normal, replay.document.options.width_km);
            self.line(
                &mut mesh,
                transform,
                edges[0],
                edges[1],
                color,
                if p.state == State::Capturing {
                    3.0
                } else {
                    1.2
                },
            );
            if !self.settings.show_fan {
                continue;
            }
            if self.projection.is_some() {
                let r = leo_sim::geometry::norm(p.satellite);
                let subpoint = leo_sim::geometry::scale(p.satellite, 1.0 / r);
                for edge in edges {
                    self.line(
                        &mut mesh,
                        transform,
                        subpoint,
                        edge,
                        color.gamma_multiply(0.6),
                        1.0,
                    );
                }
            } else {
                let sat =
                    project_surface(Vector3::from(p.satellite), &self.rotation, self.radius, 1.0);
                for edge in edges {
                    let ground = project_surface(
                        Vector3::from(edge),
                        &self.rotation,
                        self.radius,
                        self.polar_scale,
                    );
                    if sat.1 >= 0.0 && ground.1 >= 0.0 {
                        line_quad(
                            &mut mesh,
                            Self::screen(transform, sat.0),
                            Self::screen(transform, ground.0),
                            color.gamma_multiply(0.5),
                            1.0,
                        );
                    }
                }
            }
        }
        mesh
    }
}
impl PlotItem for Overlay<'_> {
    fn shapes(&self, _: &egui::Ui, transform: &PlotTransform, shapes: &mut Vec<Shape>) {
        shapes.push(Shape::mesh(self.mesh(transform)));
    }
    fn initialize(&mut self, _: RangeInclusive<f64>) {}
    fn color(&self) -> Color32 {
        Color32::LIGHT_GREEN
    }
    fn allow_hover(&self) -> bool {
        false
    }
    fn geometry(&self) -> PlotGeometry<'_> {
        PlotGeometry::None
    }
    fn bounds(&self) -> PlotBounds {
        PlotBounds::NOTHING
    }
    fn base(&self) -> &PlotItemBase {
        &self.base
    }
    fn base_mut(&mut self) -> &mut PlotItemBase {
        &mut self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reach_is_visible_during_idle_and_does_not_follow_sensor_steering() {
        let mut scene = crate::schedule_playback::tests::fixture();
        let transform = PlotTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0)),
            PlotBounds::from_min_max([-180.0, -90.0], [180.0, 90.0]),
            [false, false],
        );
        let projection = crate::projection::ProjectionKind::Equirectangular;
        let reach = |scene: &Scene| {
            let overlay = Overlay::projected(scene, 50.0, projection.instance());
            let mut mesh = Mesh::default();
            overlay.reach_mesh(&mut mesh, &transform);
            mesh
        };
        let before = reach(&scene);
        assert!(!before.vertices.is_empty());
        assert!(before
            .vertices
            .iter()
            .any(|v| v.color == crate::pushbroom::OBSERVABLE_COLOR));
        scene.replay.document.satellites[0].captures[0].mode = 0;
        let after = reach(&scene);
        assert_eq!(
            before.vertices, after.vertices,
            "Access is centered on the ground track, independent of chosen pointing"
        );
        let mut hidden = Mesh::default();
        Overlay::projected(&scene, 50.0, projection.instance())
            .with_settings(crate::pushbroom::Settings::default())
            .reach_mesh(&mut hidden, &transform);
        assert!(hidden.vertices.is_empty());
        assert_eq!(scene.replay.captured_segments(50.0), 0);
    }
    #[test]
    fn sensor_fan_shows_idle_moving_settling_and_capturing_on_globe_and_map() {
        let mut scene = crate::schedule_playback::tests::fixture();
        let mode = scene
            .replay
            .document
            .modes
            .iter()
            .position(|m| m.along_km == 0.0 && (m.cross_km - 11.79).abs() < 1e-8)
            .unwrap();
        scene.replay.document.satellites[0].captures[0].mode = mode;
        let nadir = scene
            .replay
            .document
            .modes
            .iter()
            .find(|m| m.along_km == 0.0 && m.cross_km == 0.0)
            .unwrap();
        let duration = nadir.transition_s(
            &scene.replay.document.modes[mode],
            &scene.replay.document.options,
        );
        let moving = 100.0 - duration + (duration - scene.replay.document.options.settle_s) * 0.5;
        let settings = crate::pushbroom::Settings {
            show_fan: true,
            ..Default::default()
        };
        let map_transform = PlotTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0)),
            PlotBounds::from_min_max([-180.0, -90.0], [180.0, 90.0]),
            [false, false],
        );
        let globe_transform = PlotTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 500.0)),
            PlotBounds::from_min_max([-1.2, -1.2], [1.2, 1.2]),
            [false, false],
        );
        for (t, state, color) in [
            (50.0, State::Idle, Color32::from_rgb(160, 170, 185)),
            (moving, State::Slewing, Color32::from_rgb(255, 185, 65)),
            (99.0, State::Settling, Color32::from_rgb(255, 185, 65)),
            (100.5, State::Capturing, Color32::from_rgb(100, 255, 145)),
        ] {
            let p = scene
                .replay
                .pointing(&scene.replay.document.satellites[0], t);
            assert_eq!(p.state, state);
            let rotation =
                crate::math::lat_lon_to_matrix(p.centre[1].asin(), -p.centre[2].atan2(p.centre[0]));
            let globe = Overlay::new(&scene, t, rotation, 1.0, 0.0)
                .with_settings(settings)
                .mesh(&globe_transform);
            let map = Overlay::projected(
                &scene,
                t,
                crate::projection::ProjectionKind::Equirectangular.instance(),
            )
            .with_settings(settings)
            .mesh(&map_transform);
            for mesh in [&globe, &map] {
                assert!(
                    mesh.vertices.iter().any(|v| v.color == color),
                    "Missing state cue for {state:?}"
                );
                assert!(mesh.vertices.iter().all(|v| v.pos.is_finite()));
            }
        }
        let hidden = Overlay::projected(
            &scene,
            50.0,
            crate::projection::ProjectionKind::Equirectangular.instance(),
        )
        .with_settings(crate::pushbroom::Settings::default())
        .mesh(&map_transform);
        assert!(hidden.vertices.is_empty());
    }
    #[test]
    fn acquired_mesh_is_seekable_and_does_not_bridge_idle_intervals() {
        let scene = crate::schedule_playback::tests::fixture();
        let transform = PlotTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0)),
            PlotBounds::from_min_max([-180.0, -90.0], [180.0, 90.0]),
            [false, false],
        );
        let projection = crate::projection::ProjectionKind::Equirectangular;
        let hidden = crate::pushbroom::Settings::default();
        let mesh = |time| {
            Overlay::projected(&scene, time, projection.instance())
                .with_settings(hidden)
                .mesh(&transform)
        };
        assert!(mesh(0.0).vertices.is_empty());
        assert!(mesh(99.0).vertices.is_empty());
        assert!(!mesh(100.5).vertices.is_empty());
        // At idle timestamps only the persistent strips are present; a seek
        // across the gap must not connect two unrelated capture footprints.
        assert_eq!(mesh(101.0).vertices.len(), mesh(109.0).vertices.len());
        assert!(mesh(111.0).vertices.len() > mesh(109.0).vertices.len());
        assert!(mesh(111.0 + super::super::TRAIL_SECONDS + 1.0)
            .vertices
            .is_empty());
        assert!(mesh(0.0).vertices.is_empty());
    }
}
