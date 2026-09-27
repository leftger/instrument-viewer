use std::collections::VecDeque;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use crate::palette::Palette;
use crate::waveform::ChannelTrace;

/// Interactive 3D Spectrogram / Waterfall view for sweeps and waveform history.
#[derive(Debug, Clone)]
pub struct WaterfallView {
    pub history: VecDeque<Vec<[f64; 2]>>,
    pub max_history: usize,
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub height_scale: f32,
    pub palette: Palette,
    pub is_active: bool,
}

impl Default for WaterfallView {
    fn default() -> Self {
        Self {
            history: VecDeque::with_capacity(32),
            max_history: 24,
            yaw: -0.65,
            pitch: 0.55,
            zoom: 380.0,
            pan_x: 0.0,
            pan_y: 20.0,
            height_scale: 120.0,
            palette: Palette::Turbo,
            is_active: false,
        }
    }
}

impl WaterfallView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_trace(&mut self, trace: &ChannelTrace) {
        if trace.points.is_empty() {
            return;
        }

        // Subsample trace down to max 256 points for smooth real-time 3D tessellation
        let step = (trace.points.len() / 256).max(1);
        let decimated: Vec<[f64; 2]> = trace.points.iter().step_by(step).copied().collect();

        if self.history.len() >= self.max_history {
            self.history.pop_front();
        }
        self.history.push_back(decimated);
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn reset_view(&mut self) {
        self.yaw = -0.65;
        self.pitch = 0.55;
        self.zoom = 380.0;
        self.pan_x = 0.0;
        self.pan_y = 20.0;
    }

    /// Renders the 3D waterfall plot into the egui UI.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());

        // Mouse Drag Interaction
        if response.dragged() {
            let delta = response.drag_delta();
            let input = ui.input(|i| i.clone());

            if input.modifiers.ctrl
                || input.pointer.button_down(egui::PointerButton::Middle)
                || input.pointer.button_down(egui::PointerButton::Secondary)
            {
                self.pan_x += delta.x;
                self.pan_y += delta.y;
            } else {
                self.yaw += delta.x * 0.01;
                self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.4, 1.4);
            }
        }

        // Mouse Wheel Zoom
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            self.zoom = (self.zoom + scroll * 0.8).clamp(80.0, 1200.0);
        }

        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(16, 18, 24));

        if self.history.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No waterfall sweep history captured yet (Capture or start Auto)",
                egui::FontId::monospace(14.0),
                Color32::from_rgb(120, 125, 140),
            );
            return;
        }

        let center = rect.center() + Vec2::new(self.pan_x, self.pan_y);

        // Find min and max Y across history for normalization
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;

        for sweep in &self.history {
            for p in sweep {
                min_x = min_x.min(p[0]);
                max_x = max_x.max(p[0]);
                min_y = min_y.min(p[1]);
                max_y = max_y.max(p[1]);
            }
        }

        let x_span = (max_x - min_x).max(1e-9);
        let y_span = (max_y - min_y).max(1e-9);
        let z_count = self.history.len();

        let cy = self.yaw.cos();
        let sy = self.yaw.sin();
        let cp = self.pitch.cos();
        let sp = self.pitch.sin();

        let project = |x_norm: f32, y_norm: f32, z_norm: f32| -> Pos2 {
            // Center coordinates in [-0.5, 0.5]
            let wx = (x_norm - 0.5) * 260.0;
            let wy = (y_norm - 0.5) * self.height_scale;
            let wz = (z_norm - 0.5) * 260.0;

            // Rotation around Y (yaw)
            let x1 = wx * cy - wz * sy;
            let z1 = wx * sy + wz * cy;

            // Rotation around X (pitch)
            let y2 = wy * cp - z1 * sp;
            let z2 = wy * sp + z1 * cp;

            // Perspective projection
            let dist = 600.0;
            let factor = (dist / (dist + z2)).max(0.2);

            let sx = center.x + x1 * factor * (self.zoom / 300.0);
            let sy = center.y - y2 * factor * (self.zoom / 300.0);
            Pos2::new(sx, sy)
        };

        // Draw 3D wireframe / ribbon sweeps
        for (z_idx, sweep) in self.history.iter().enumerate() {
            if sweep.len() < 2 {
                continue;
            }
            let z_norm = if z_count > 1 {
                z_idx as f32 / (z_count - 1) as f32
            } else {
                0.5
            };

            let mut prev_pos: Option<Pos2> = None;
            for p in sweep {
                let x_norm = ((p[0] - min_x) / x_span) as f32;
                let y_norm = ((p[1] - min_y) / y_span) as f32;
                let screen_pos = project(x_norm, y_norm, z_norm);

                let rgb = self.palette.map_normalized(y_norm);
                // Depth fade
                let alpha = (120.0 + z_norm * 135.0).clamp(40.0, 255.0) as u8;
                let color = Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], alpha);

                if let Some(prev) = prev_pos {
                    painter.line_segment([prev, screen_pos], Stroke::new(1.8_f32, color));
                }
                prev_pos = Some(screen_pos);
            }
        }

        // Draw HUD overlay
        let hud_rect =
            Rect::from_min_size(rect.min + Vec2::new(10.0, 10.0), Vec2::new(260.0, 52.0));
        painter.rect_filled(hud_rect, 4.0, Color32::from_black_alpha(160));
        painter.text(
            hud_rect.min + Vec2::new(8.0, 8.0),
            egui::Align2::LEFT_TOP,
            format!("3D Waterfall ({} sweeps)", self.history.len()),
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
        painter.text(
            hud_rect.min + Vec2::new(8.0, 28.0),
            egui::Align2::LEFT_TOP,
            "Drag: Rotate | Ctrl/R-Drag: Pan | Scroll: Zoom",
            egui::FontId::monospace(10.5),
            Color32::from_rgb(180, 185, 200),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waterfall_push_and_capacity() {
        let mut wf = WaterfallView::new();
        wf.max_history = 3;

        let trace = ChannelTrace {
            channel: "CH1".to_string(),
            x_unit: "Hz".to_string(),
            y_unit: "dBm".to_string(),
            points: vec![[1e6, -50.0], [2e6, -20.0], [3e6, -45.0]],
        };

        wf.push_trace(&trace);
        assert_eq!(wf.history.len(), 1);

        wf.push_trace(&trace);
        wf.push_trace(&trace);
        assert_eq!(wf.history.len(), 3);

        wf.push_trace(&trace);
        assert_eq!(wf.history.len(), 3);

        wf.clear();
        assert!(wf.history.is_empty());
    }

    #[test]
    fn waterfall_rendering_and_reset() {
        let mut wf = WaterfallView::new();
        let ctx = egui::Context::default();

        // Render empty waterfall
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                wf.show(ui);
            });
        });

        // Push traces and render populated waterfall
        let trace = ChannelTrace {
            channel: "CH1".to_string(),
            x_unit: "Hz".to_string(),
            y_unit: "dBm".to_string(),
            points: vec![[1e6, -50.0], [2e6, -20.0], [3e6, -45.0]],
        };
        wf.push_trace(&trace);
        wf.push_trace(&trace);

        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                wf.show(ui);
            });
        });

        assert_eq!(wf.history.len(), 2);
        wf.reset_view();
        assert!((wf.zoom - 380.0).abs() < 1e-3);
    }
}
