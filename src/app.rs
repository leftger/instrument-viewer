use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;
use egui_plot::{Legend, Line, Plot, PlotPoints, VLine};

use crate::config::{ConfigSection, InstrumentConfig};
use crate::export::ExportOptions;
use crate::measure;
use crate::plotdata;
use crate::prefs::{self, Prefs};
use crate::waveform::{demo_trace, ChannelTrace};
use crate::worker::{Cmd, Msg, Worker};

const CHANNELS: &[&str] = &["CH1", "CH2", "CH3", "CH4"];

pub struct ViewerApp {
    host: String,
    port: String,
    status: String,
    idn: Option<String>,
    config: Option<InstrumentConfig>,
    selected_channel: usize,
    traces: Vec<ChannelTrace>,
    auto: bool,
    /// Seconds between automatic captures. Deliberately unhurried: this
    /// instrument does not like being pushed.
    auto_interval: f64,
    /// A fetch is in flight; do not queue another or the requests pile up.
    pending: bool,
    /// When `pending` was raised, so a lost reply cannot lock the UI forever.
    pending_since: Option<f64>,
    last_fetch: f64,
    /// Frame counter and last frame duration, shown so a stalled or merely slow
    /// UI can be told apart.
    frames: u64,
    frame_ms: f32,
    /// UI clock time before which reconnecting is pointless, after a wedge.
    retry_at: f64,
    raw_command: String,
    raw_response: String,
    csv_wide: bool,
    cursors_on: bool,
    cursor_a: Option<f64>,
    cursor_b: Option<f64>,
    pending_png: Option<PathBuf>,
    /// Rescale to the data on every frame, so each capture fits the window.
    auto_fit: bool,
    zoom_x: bool,
    zoom_y: bool,
    /// Wheel/two-finger scroll zooms instead of panning. Mac trackpads have no
    /// wheel-modifier convention that reaches egui as a per-axis zoom.
    scroll_zooms: bool,
    /// Zoom factor queued by the toolbar buttons, applied inside the plot.
    zoom_request: Option<egui::Vec2>,
    fit_request: bool,
    /// Points actually sent to the plot last frame, after decimation.
    drawn_points: usize,
    worker: Worker,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, host: String, port: u16, prefs: Prefs) -> Self {
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(move || ctx.request_repaint());
        Self {
            host,
            port: port.to_string(),
            status: "Not connected.".into(),
            idn: None,
            config: None,
            selected_channel: 0,
            traces: vec![demo_trace("CH1")],
            auto: false,
            auto_interval: prefs.auto_interval,
            pending: false,
            pending_since: None,
            last_fetch: 0.0,
            frames: 0,
            frame_ms: 0.0,
            retry_at: 0.0,
            raw_command: "*IDN?".into(),
            raw_response: String::new(),
            csv_wide: prefs.csv_wide,
            cursors_on: false,
            cursor_a: None,
            cursor_b: None,
            pending_png: None,
            auto_fit: true,
            zoom_x: true,
            zoom_y: true,
            scroll_zooms: prefs.scroll_zooms,
            zoom_request: None,
            fit_request: false,
            drawn_points: 0,
            worker,
        }
    }

    fn persist(&self) {
        prefs::save(&Prefs {
            host: self.host.clone(),
            port: self.port.clone(),
            auto_interval: self.auto_interval,
            csv_wide: self.csv_wide,
            scroll_zooms: self.scroll_zooms,
        });
    }

    fn addr(&self) -> String {
        format!("{}:{}", self.host.trim(), self.port.trim())
    }

    fn selected(&self) -> Vec<String> {
        self.config
            .as_ref()
            .map(|config| {
                CHANNELS
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| config.channels[*i].enabled)
                    .map(|(_, c)| c.to_string())
                    .collect()
            })
            .unwrap_or_else(|| vec!["CH1".into()])
    }

    fn request_fetch(&mut self) {
        if self.pending || self.idn.is_none() {
            return;
        }
        let channels = self.selected();
        if channels.is_empty() {
            self.status = "No channels selected.".into();
            return;
        }
        self.pending = true;
        self.worker.send(Cmd::Fetch { channels });
    }

    fn request_sequence_fetch(&mut self) {
        if self.pending || self.idn.is_none() {
            return;
        }
        let channels = self.selected();
        if channels.is_empty() {
            self.status = "No channels selected.".into();
            return;
        }
        self.auto = false;
        self.pending = true;
        self.worker.send(Cmd::FetchSequence { channels });
    }

    fn export_opts(&self, format: crate::export::ExportFormat) -> ExportOptions<'_> {
        ExportOptions {
            traces: &self.traces,
            idn: self.idn.as_deref(),
            settings: self.config.as_ref(),
            format,
            csv_wide: self.csv_wide,
            cursor_a: self.cursor_a.filter(|_| self.cursors_on),
            cursor_b: self.cursor_b.filter(|_| self.cursors_on),
        }
    }

    fn export_traces(&mut self, format: crate::export::ExportFormat) {
        let name = format!("mdo-capture.{}", format.extension());
        let mut dialog = rfd::FileDialog::new().set_file_name(&name);
        dialog = match format {
            crate::export::ExportFormat::Csv => dialog.add_filter("CSV", &["csv"]),
            crate::export::ExportFormat::Json => dialog.add_filter("JSON", &["json"]),
        };
        let Some(path) = dialog.save_file() else {
            return;
        };
        let opts = self.export_opts(format);
        match crate::export::write_file(&path, &opts) {
            Ok(()) => {
                self.status = format!("Wrote {}", path.display());
            }
            Err(e) => {
                self.status = format!("Export failed: {e}");
            }
        }
    }

    fn queue_zoom(&mut self, k: f32) {
        self.zoom_request = Some(axis_factor(k, egui::Vec2b::new(self.zoom_x, self.zoom_y)));
        self.auto_fit = false;
    }

    fn request_png(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("mdo-capture.png")
            .add_filter("PNG", &["png"])
            .save_file()
        else {
            return;
        };
        self.pending_png = Some(path);
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }

    fn collect_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.pending_png.clone() else {
            return;
        };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|ev| match ev {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else {
            return;
        };
        self.pending_png = None;
        match write_png(&path, &image) {
            Ok(()) => self.status = format!("Wrote {}", path.display()),
            Err(e) => self.status = format!("PNG failed: {e}"),
        }
    }

    fn show_measurements(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("meas").show(ctx, |ui| {
            ui.add_space(2.0);
            if self.cursors_on {
                ui.horizontal_wrapped(|ui| {
                    let a = self.cursor_a;
                    let b = self.cursor_b;
                    ui.label(format!(
                        "A: {}",
                        a.map(|t| measure::format_si(t, "s"))
                            .unwrap_or_else(|| "click".into())
                    ));
                    ui.label(format!(
                        "B: {}",
                        b.map(|t| measure::format_si(t, "s"))
                            .unwrap_or_else(|| "right-click".into())
                    ));
                    if let (Some(t1), Some(t2)) = (a, b) {
                        let dt = t2 - t1;
                        ui.strong(format!("Δt {}", measure::format_si(dt, "s")));
                        if dt.abs() > f64::EPSILON {
                            ui.strong(format!("1/Δt {}", measure::format_si(1.0 / dt.abs(), "Hz")));
                        }
                    }
                    if let Some(trace) = self.traces.first() {
                        if let Some(t) = a {
                            if let Some(v) = measure::value_at(trace, t) {
                                ui.label(format!(
                                    "A@{} {}",
                                    trace.channel,
                                    measure::format_si(v, &trace.y_unit)
                                ));
                            }
                        }
                        if let Some(t) = b {
                            if let Some(v) = measure::value_at(trace, t) {
                                ui.label(format!(
                                    "B@{} {}",
                                    trace.channel,
                                    measure::format_si(v, &trace.y_unit)
                                ));
                            }
                        }
                    }
                });
            }
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    for trace in &self.traces {
                        let Some(m) = measure::measure(trace) else {
                            continue;
                        };
                        ui.group(|ui| {
                            ui.label(egui::RichText::new(&trace.channel).strong());
                            ui.label(format!("min {}", measure::format_si(m.min, &trace.y_unit)));
                            ui.label(format!("max {}", measure::format_si(m.max, &trace.y_unit)));
                            ui.label(format!(
                                "pk-pk {}",
                                measure::format_si(m.pk_pk, &trace.y_unit)
                            ));
                            ui.label(format!(
                                "mean {}",
                                measure::format_si(m.mean, &trace.y_unit)
                            ));
                            ui.label(format!("rms {}", measure::format_si(m.rms, &trace.y_unit)));
                            match (m.period_s, m.frequency_hz) {
                                (Some(p), Some(f)) => {
                                    ui.label(format!(
                                        "{}  {}",
                                        measure::format_si(p, "s"),
                                        measure::format_si(f, "Hz")
                                    ));
                                }
                                _ => {
                                    ui.label("period —");
                                }
                            }
                        });
                    }
                    let raw: usize = self.traces.iter().map(|t| t.points.len()).sum();
                    if raw > 0 {
                        ui.label(format!("plotted {} / {raw} pts", self.drawn_points))
                            .on_hover_text(
                                "Traces are min/max reduced to the plot width; \
                                 peaks are preserved and exports use full resolution.",
                            );
                    }
                    // Liveness: if this keeps counting, the UI thread is alive
                    // and any apparent freeze is elsewhere.
                    ui.label(format!("frame {} · {:.0} ms", self.frames, self.frame_ms))
                        .on_hover_text("Frame counter and frame time");
                });
            });
            ui.add_space(2.0);
        });
    }

    fn pump(&mut self, now: f64) {
        while let Some(msg) = self.worker.try_recv() {
            match msg {
                Msg::Status(s) => self.status = s,
                Msg::Connected(idn) => {
                    self.status = format!("Connected: {idn}");
                    self.idn = Some(idn);
                    self.pending = true;
                    self.worker.send(Cmd::ReadConfig);
                }
                Msg::Disconnected => {
                    self.idn = None;
                    self.auto = false;
                    self.pending = false;
                    self.config = None;
                    self.status = "Disconnected.".into();
                }
                Msg::Traces(t) => {
                    self.pending = false;
                    let n: usize = t.iter().map(|x| x.points.len()).sum();
                    self.status = format!("{n} samples across {} channel(s)", t.len());
                    self.traces = t;
                }
                Msg::Config(config) => {
                    self.pending = false;
                    self.config = Some(*config);
                    self.status = "Instrument settings synchronized.".into();
                }
                Msg::Applied(status) => {
                    self.status = status;
                    self.pending = true;
                    self.worker.send(Cmd::ReadConfig);
                }
                Msg::RawResponse(response) => {
                    self.pending = false;
                    self.raw_response = response;
                    self.status = "SCPI query complete.".into();
                }
                Msg::Error(e) => {
                    self.pending = false;
                    self.auto = false;
                    // Reconnecting into a wedged socket server only prolongs it.
                    if e.contains("wedged") {
                        self.retry_at = now + 30.0;
                    }
                    self.status = format!("Error: {e}");
                }
            }
        }
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        self.frames += 1;
        self.frame_ms = ctx.input(|i| i.unstable_dt) * 1000.0;
        self.pump(now);
        self.collect_screenshot(ctx);

        // A command whose reply never arrives must not strand the UI, so give
        // up on it rather than leaving every button disabled.
        if self.pending {
            let started = *self.pending_since.get_or_insert(now);
            if now - started > 20.0 {
                self.pending = false;
                self.pending_since = None;
                self.status =
                    "No reply after 20 s; releasing the UI. Disconnect and reconnect.".into();
            }
        } else {
            self.pending_since = None;
        }

        // Heartbeat. Waking only on the worker's cross-thread repaint request
        // leaves the window looking dead if that wakeup is ever missed.
        if self.pending || self.idn.is_some() || now < self.retry_at {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        if self.auto && !self.pending {
            if now - self.last_fetch >= self.auto_interval {
                self.last_fetch = now;
                self.request_fetch();
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let connected = self.idn.is_some();
                ui.add_enabled_ui(!connected, |ui| {
                    ui.label("Host");
                    ui.add(egui::TextEdit::singleline(&mut self.host).desired_width(130.0));
                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(50.0));
                });

                if !connected {
                    let cooling = now < self.retry_at;
                    let label = if cooling {
                        format!("Wait {:.0}s", self.retry_at - now)
                    } else {
                        "Connect".to_string()
                    };
                    if ui
                        .add_enabled(!self.pending && !cooling, egui::Button::new(label))
                        .clicked()
                    {
                        self.persist();
                        self.pending = true;
                        self.worker.send(Cmd::Connect { addr: self.addr() });
                    }
                // Always live, even mid-command: this is the way out when the
                // instrument stops answering.
                } else if ui.button("Disconnect").clicked() {
                    self.auto = false;
                    self.worker.send(Cmd::Disconnect);
                }

                ui.add_enabled_ui(connected && !self.pending, |ui| {
                    if ui.button("Fetch").clicked() {
                        self.request_fetch();
                    }
                    if ui
                        .button("Sequence")
                        .on_hover_text("STOPAFTER SEQUENCE, wait, then fetch")
                        .clicked()
                    {
                        self.request_sequence_fetch();
                    }
                });
                ui.add_enabled_ui(connected, |ui| {
                    ui.checkbox(&mut self.auto, "Auto");
                    let before = self.auto_interval;
                    ui.add(
                        egui::DragValue::new(&mut self.auto_interval)
                            .speed(0.1)
                            .range(0.5..=30.0)
                            .suffix(" s"),
                    )
                    .on_hover_text("Seconds between automatic captures");
                    if (self.auto_interval - before).abs() > f64::EPSILON {
                        self.persist();
                    }
                });

                if ui.button("Demo").clicked() {
                    self.traces = self.selected().iter().map(|c| demo_trace(c)).collect();
                    self.status = "Demo waveform (no instrument).".into();
                }

                ui.checkbox(&mut self.cursors_on, "Cursors")
                    .on_hover_text("Click plot: left sets A, right/shift sets B");

                if ui
                    .add_enabled(!self.traces.is_empty(), egui::Button::new("CSV"))
                    .on_hover_text("Save the plotted traces as CSV")
                    .clicked()
                {
                    self.export_traces(crate::export::ExportFormat::Csv);
                }
                if ui.checkbox(&mut self.csv_wide, "Wide").changed() {
                    self.persist();
                }
                if ui
                    .add_enabled(!self.traces.is_empty(), egui::Button::new("JSON"))
                    .on_hover_text("Save traces, measurements, and settings as JSON")
                    .clicked()
                {
                    self.export_traces(crate::export::ExportFormat::Json);
                }
                if ui
                    .button("PNG")
                    .on_hover_text("Save a window screenshot")
                    .clicked()
                {
                    self.request_png(ctx);
                }

                if self.pending {
                    ui.spinner();
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("View");
                if ui
                    .selectable_label(self.auto_fit, "Autoscale")
                    .on_hover_text("Refit both axes to the data on every capture")
                    .clicked()
                {
                    self.auto_fit = !self.auto_fit;
                }
                if ui.button("Fit now").clicked() {
                    self.fit_request = true;
                }
                ui.separator();

                ui.label("Zoom axes");
                ui.checkbox(&mut self.zoom_x, "X");
                ui.checkbox(&mut self.zoom_y, "Y");
                ui.separator();

                if ui.button("−").on_hover_text("Zoom out").clicked() {
                    self.queue_zoom(1.0 / 1.4);
                }
                if ui.button("+").on_hover_text("Zoom in").clicked() {
                    self.queue_zoom(1.4);
                }
                ui.separator();

                ui.label("Y only");
                if ui.button("−Y").clicked() {
                    self.zoom_request = Some(egui::Vec2::new(1.0, 1.0 / 1.4));
                    self.auto_fit = false;
                }
                if ui.button("+Y").clicked() {
                    self.zoom_request = Some(egui::Vec2::new(1.0, 1.4));
                    self.auto_fit = false;
                }
                ui.separator();

                if ui
                    .checkbox(&mut self.scroll_zooms, "Scroll zooms")
                    .on_hover_text("Off: two-finger scroll pans. On: it zooms the enabled axes.")
                    .changed()
                {
                    self.persist();
                }
                ui.label("Drag pans · right-drag box-zooms");
            });

            ui.label(&self.status);
            ui.add_space(4.0);
        });

        self.show_controls(ctx);
        self.show_measurements(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            let x_unit = self.traces.first().map_or("s", |t| t.x_unit.as_str());
            let y_unit = self.traces.first().map_or("V", |t| t.y_unit.as_str());
            let mut clicked = None;
            let mut secondary = false;
            let zoom_request = self.zoom_request.take();
            let fit_request = std::mem::take(&mut self.fit_request);
            let axes = egui::Vec2b::new(self.zoom_x, self.zoom_y);

            // Read the wheel before the plot so auto-fit can be released in the
            // same frame; otherwise it would snap the view back immediately.
            let scroll = if self.scroll_zooms && ui.rect_contains_pointer(ui.max_rect()) {
                ctx.input(|i| i.smooth_scroll_delta.y)
            } else {
                0.0
            };
            let scroll_factor = (scroll != 0.0).then(|| axis_factor((scroll * 0.004).exp(), axes));
            if zoom_request.is_some() || scroll_factor.is_some() {
                self.auto_fit = false;
            }
            let do_fit = fit_request || self.auto_fit;
            let mut dragged = false;
            let width_px = ui.available_width() as f64;
            let mut drawn = 0usize;
            Plot::new("mdo")
                .legend(Legend::default())
                .x_axis_label(x_unit)
                .y_axis_label(y_unit)
                .allow_zoom(axes)
                .allow_drag(true)
                .allow_boxed_zoom(true)
                // Manual scroll handling below, so the wheel can zoom per axis.
                .allow_scroll(!self.scroll_zooms)
                .show(ui, |plot_ui| {
                    // Last frame's bounds; good enough to size this frame's detail.
                    let visible = plot_ui.plot_bounds();
                    for trace in &self.traces {
                        let reduced = plotdata::prepare(
                            &trace.points,
                            visible.min()[0],
                            visible.max()[0],
                            width_px,
                            !do_fit,
                        );
                        drawn += reduced.len();
                        plot_ui.line(Line::new(trace.channel.clone(), PlotPoints::from(reduced)));
                    }
                    if self.cursors_on {
                        if let Some(t) = self.cursor_a {
                            plot_ui.vline(VLine::new("A", t));
                        }
                        if let Some(t) = self.cursor_b {
                            plot_ui.vline(VLine::new("B", t));
                        }
                    }

                    if do_fit {
                        plot_ui.set_auto_bounds(true);
                    }
                    if let Some(factor) = zoom_request {
                        let center = plot_ui.plot_bounds().center();
                        plot_ui.zoom_bounds(factor, center);
                    }
                    if let Some(factor) = scroll_factor {
                        plot_ui.zoom_bounds_around_hovered(factor);
                    }

                    let resp = plot_ui.response();
                    dragged = resp.dragged();
                    if resp.clicked() || resp.secondary_clicked() {
                        clicked = plot_ui.pointer_coordinate().map(|p| p.x);
                        secondary =
                            resp.secondary_clicked() || resp.ctx.input(|i| i.modifiers.shift);
                    }
                });
            self.drawn_points = drawn;
            if dragged {
                self.auto_fit = false;
            }
            if self.cursors_on {
                if let Some(t) = clicked {
                    if secondary {
                        self.cursor_b = Some(t);
                    } else {
                        self.cursor_a = Some(t);
                    }
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist();
    }
}

impl ViewerApp {
    fn send_config(&mut self, section: ConfigSection) {
        if self.pending || self.idn.is_none() {
            return;
        }
        self.pending = true;
        self.worker.send(Cmd::ApplyConfig(section));
    }

    fn refresh_config(&mut self) {
        if self.pending || self.idn.is_none() {
            return;
        }
        self.pending = true;
        self.worker.send(Cmd::ReadConfig);
    }

    fn show_controls(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("controls")
            .resizable(true)
            .default_width(315.0)
            .show(ctx, |ui| {
                ui.heading("Instrument");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.pending && self.idn.is_some(),
                            egui::Button::new("Refresh"),
                        )
                        .clicked()
                    {
                        self.refresh_config();
                    }
                    if ui
                        .add_enabled(
                            !self.pending && self.idn.is_some(),
                            egui::Button::new("Autoset"),
                        )
                        .clicked()
                    {
                        self.pending = true;
                        self.worker.send(Cmd::Autoset);
                    }
                });
                ui.separator();

                let Some(mut config) = self.config.clone() else {
                    ui.label("Connect to read controls.");
                    return;
                };

                egui::CollapsingHeader::new("Channels")
                    .default_open(true)
                    .show(ui, |ui| {
                        egui::ComboBox::from_label("Channel")
                            .selected_text(CHANNELS[self.selected_channel])
                            .show_ui(ui, |ui| {
                                for (index, name) in CHANNELS.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected_channel, index, *name);
                                }
                            });

                        let index = self.selected_channel;
                        let ch = &mut config.channels[index];
                        ui.checkbox(&mut ch.enabled, "Displayed / fetched");
                        value_row(ui, "Scale (V/div)", &mut ch.scale, 0.01);
                        value_row(ui, "Position (div)", &mut ch.position, 0.1);
                        value_row(ui, "Offset (V)", &mut ch.offset, 0.01);

                        egui::ComboBox::from_label("Coupling")
                            .selected_text(&ch.coupling)
                            .show_ui(ui, |ui| {
                                for value in ["DC", "AC", "DCREJECT"] {
                                    ui.selectable_value(&mut ch.coupling, value.into(), value);
                                }
                            });

                        egui::ComboBox::from_label("Input")
                            .selected_text(if ch.termination_ohms < 1000.0 {
                                "50 Ω"
                            } else {
                                "1 MΩ"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut ch.termination_ohms, 50.0, "50 Ω");
                                ui.selectable_value(&mut ch.termination_ohms, 1e6, "1 MΩ");
                            });
                        if ch.termination_ohms < 1000.0 {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                "50 Ω physically loads the input; verify source voltage.",
                            );
                        }

                        let mut attenuation = gain_to_attenuation(ch.probe_gain);
                        egui::ComboBox::from_label("Probe")
                            .selected_text(format!("{attenuation}×"))
                            .show_ui(ui, |ui| {
                                for value in [1.0, 10.0, 100.0, 1000.0] {
                                    ui.selectable_value(
                                        &mut attenuation,
                                        value,
                                        format!("{value}×"),
                                    );
                                }
                            });
                        ch.probe_gain = 1.0 / attenuation;
                        ui.label(format!("Detected: {}", ch.probe_type));

                        egui::ComboBox::from_label("Bandwidth")
                            .selected_text(format_bandwidth(ch.bandwidth_hz))
                            .show_ui(ui, |ui| {
                                for (label, value) in [
                                    ("20 MHz", 20e6),
                                    ("100 MHz", 100e6),
                                    ("200 MHz / Full", 200e6),
                                ] {
                                    ui.selectable_value(&mut ch.bandwidth_hz, value, label);
                                }
                            });

                        if ui
                            .add_enabled(!self.pending, egui::Button::new("Apply channel"))
                            .clicked()
                        {
                            let section = ConfigSection::Channel(index, ch.clone());
                            self.config = Some(config.clone());
                            self.send_config(section);
                        }
                    });

                egui::CollapsingHeader::new("Horizontal")
                    .default_open(true)
                    .show(ui, |ui| {
                        let h = &mut config.horizontal;
                        value_row(ui, "Time/div (s)", &mut h.scale, 1e-6);
                        value_row(ui, "Position (%)", &mut h.position, 1.0);
                        egui::ComboBox::from_label("Record length")
                            .selected_text(h.record_length.to_string())
                            .show_ui(ui, |ui| {
                                for value in
                                    [1_000, 10_000, 100_000, 1_000_000, 5_000_000, 10_000_000]
                                {
                                    ui.selectable_value(
                                        &mut h.record_length,
                                        value,
                                        format_count(value),
                                    );
                                }
                            });
                        if ui
                            .add_enabled(!self.pending, egui::Button::new("Apply horizontal"))
                            .clicked()
                        {
                            let section = ConfigSection::Horizontal(h.clone());
                            self.config = Some(config.clone());
                            self.send_config(section);
                        }
                    });

                egui::CollapsingHeader::new("Edge trigger")
                    .default_open(true)
                    .show(ui, |ui| {
                        let t = &mut config.trigger;
                        combo_string(ui, "Mode", &mut t.mode, &["AUTO", "NORMAL"]);
                        combo_string(ui, "Source", &mut t.source, CHANNELS);
                        combo_string(ui, "Slope", &mut t.slope, &["RISE", "FALL", "EITHER"]);
                        combo_string(
                            ui,
                            "Coupling",
                            &mut t.coupling,
                            &["DC", "AC", "HFREJ", "LFREJ", "NOISEREJ"],
                        );
                        value_row(ui, "Level (V)", &mut t.level, 0.01);
                        if ui
                            .add_enabled(!self.pending, egui::Button::new("Apply trigger"))
                            .clicked()
                        {
                            let section = ConfigSection::Trigger(t.clone());
                            self.config = Some(config.clone());
                            self.send_config(section);
                        }
                    });

                egui::CollapsingHeader::new("Acquisition")
                    .default_open(true)
                    .show(ui, |ui| {
                        let a = &mut config.acquisition;
                        combo_string(
                            ui,
                            "Mode",
                            &mut a.mode,
                            &["SAMPLE", "PEAKDETECT", "HIRES", "AVERAGE", "ENVELOPE"],
                        );
                        combo_string(
                            ui,
                            "Stop after",
                            &mut a.stop_after,
                            &["RUNSTOP", "SEQUENCE"],
                        );
                        ui.checkbox(&mut a.running, "Running");
                        if ui
                            .add_enabled(!self.pending, egui::Button::new("Apply acquisition"))
                            .clicked()
                        {
                            let section = ConfigSection::Acquisition(a.clone());
                            self.config = Some(config.clone());
                            self.send_config(section);
                        }
                    });

                egui::CollapsingHeader::new("Raw SCPI")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.text_edit_singleline(&mut self.raw_command);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!self.pending, egui::Button::new("Query"))
                                .clicked()
                            {
                                self.pending = true;
                                self.worker.send(Cmd::RawQuery(self.raw_command.clone()));
                            }
                            if ui
                                .add_enabled(!self.pending, egui::Button::new("Write"))
                                .clicked()
                            {
                                self.pending = true;
                                self.worker.send(Cmd::RawWrite(self.raw_command.clone()));
                            }
                        });
                        ui.add(
                            egui::TextEdit::multiline(&mut self.raw_response)
                                .desired_rows(3)
                                .interactive(false),
                        );
                    });

                self.config = Some(config);
            });
    }
}

/// Apply a scalar zoom only to the axes the user left enabled.
fn axis_factor(k: f32, axes: egui::Vec2b) -> egui::Vec2 {
    egui::Vec2::new(if axes.x { k } else { 1.0 }, if axes.y { k } else { 1.0 })
}

fn write_png(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), String> {
    let [w, h] = image.size;
    let mut raw = Vec::with_capacity(w * h * 4);
    for p in &image.pixels {
        raw.extend_from_slice(&p.to_array());
    }
    let img = image::RgbaImage::from_raw(w as u32, h as u32, raw)
        .ok_or_else(|| "invalid screenshot buffer".to_string())?;
    img.save(path).map_err(|e| e.to_string())
}

fn value_row(ui: &mut egui::Ui, label: &str, value: &mut f64, speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(value).speed(speed).max_decimals(9));
    });
}

fn combo_string(ui: &mut egui::Ui, label: &str, current: &mut String, values: &[&str]) {
    egui::ComboBox::from_label(label)
        .selected_text(current.as_str())
        .show_ui(ui, |ui| {
            for value in values {
                ui.selectable_value(current, (*value).to_string(), *value);
            }
        });
}

fn gain_to_attenuation(gain: f64) -> f64 {
    if gain > 0.0 {
        1.0 / gain
    } else {
        1.0
    }
}

fn format_bandwidth(value: f64) -> String {
    format!("{:.0} MHz", value / 1e6)
}

fn format_count(value: u64) -> String {
    match value {
        1_000_000 => "1M".into(),
        5_000_000 => "5M".into(),
        10_000_000 => "10M".into(),
        100_000 => "100k".into(),
        10_000 => "10k".into(),
        1_000 => "1k".into(),
        _ => value.to_string(),
    }
}
