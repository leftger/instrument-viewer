use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;
use egui_plot::{GridMark, Legend, Line, Plot, PlotPoints, VLine};

use crate::backend::{InstrumentCapabilities, InstrumentKind, ValueChoice};
use crate::config::{ConfigSection, InstrumentConfig};
use crate::discover::FoundScope;
use crate::export::ExportOptions;
use crate::measure;
use crate::plotdata;
use crate::prefs::{self, Prefs};
use crate::stack;
use crate::waveform::{demo_trace, ChannelTrace};
use crate::worker::{Cmd, Msg, Worker};

const CHANNELS: &[&str] = &["CH1", "CH2", "CH3", "CH4"];
const STATUS_POLL_INTERVAL: f64 = 2.0;
const STATUS_POLL_BACKOFF: f64 = 4.0;

#[derive(Clone, Debug, PartialEq)]
struct SupplySample {
    t_s: f64,
    channel: String,
    volts: f64,
    amps: f64,
}

pub struct ViewerApp {
    host: String,
    port: String,
    status: String,
    idn: Option<String>,
    capabilities: Option<InstrumentCapabilities>,
    config: Option<InstrumentConfig>,
    selected_channel: usize,
    traces: Vec<ChannelTrace>,
    captured_at: Option<crate::timestamp::CaptureTime>,
    /// Stacked-view placement, one per trace. Kept with the traces rather than
    /// rebuilt per frame: it costs a pass over every captured sample.
    lanes: Vec<stack::Lane>,
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
    acquisition_status: Option<String>,
    status_poll_in_flight: bool,
    last_status_poll: f64,
    status_poll_retry_at: f64,
    raw_command: String,
    raw_response: String,
    csv_wide: bool,
    cursors_on: bool,
    cursor_a: Option<f64>,
    cursor_b: Option<f64>,
    pending_png: Option<PathBuf>,
    /// CLI screenshot destination: render a demo trace, save the window, exit.
    screenshot_out: Option<PathBuf>,
    /// UI clock when the CLI screenshot was requested, for the give-up timer.
    screenshot_requested_at: Option<f64>,
    /// Rescale to the data on every frame, so each capture fits the window.
    auto_fit: bool,
    /// Give each channel its own band and its own gain instead of sharing one
    /// volts axis.
    stacked: bool,
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
    scanning: bool,
    scan_results: Vec<FoundScope>,
    /// Supply readings taken since connect, in session time.
    supply_history: Vec<SupplySample>,
    /// UI clock at the first supply reading of this connection.
    supply_t0: Option<f64>,
    worker: Worker,
}

impl ViewerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        host: String,
        port: u16,
        prefs: Prefs,
        screenshot: Option<PathBuf>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(move || ctx.request_repaint());
        let traces = vec![demo_trace("CH1")];
        let screenshot_status = if screenshot.is_some() {
            "Demo waveform (no instrument).".into()
        } else {
            "Not connected.".into()
        };
        Self {
            lanes: stack::lanes(&traces),
            host,
            port: port.to_string(),
            status: screenshot_status,
            idn: None,
            capabilities: None,
            config: None,
            selected_channel: 0,
            traces,
            captured_at: None,
            auto: false,
            auto_interval: prefs.auto_interval,
            pending: false,
            pending_since: None,
            last_fetch: 0.0,
            frames: 0,
            frame_ms: 0.0,
            retry_at: 0.0,
            acquisition_status: None,
            status_poll_in_flight: false,
            last_status_poll: 0.0,
            status_poll_retry_at: 0.0,
            raw_command: "*IDN?".into(),
            raw_response: String::new(),
            csv_wide: prefs.csv_wide,
            cursors_on: false,
            cursor_a: None,
            cursor_b: None,
            pending_png: None,
            screenshot_out: screenshot,
            screenshot_requested_at: None,
            auto_fit: true,
            stacked: false,
            zoom_x: true,
            zoom_y: true,
            scroll_zooms: prefs.scroll_zooms,
            zoom_request: None,
            fit_request: false,
            drawn_points: 0,
            scanning: false,
            scan_results: Vec::new(),
            supply_history: Vec::new(),
            supply_t0: None,
            worker,
        }
    }

    fn is_supply(&self) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|caps| caps.kind == InstrumentKind::Supply)
    }

    fn is_multimeter(&self) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|caps| caps.kind == InstrumentKind::Multimeter)
    }

    fn clear_supply_session(&mut self) {
        self.supply_history.clear();
        self.supply_t0 = None;
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

    fn set_traces(&mut self, traces: Vec<ChannelTrace>) {
        let mixed_units = traces
            .first()
            .is_some_and(|first| traces.iter().any(|trace| trace.y_unit != first.y_unit));
        if mixed_units {
            // A shared y axis cannot meaningfully compare volts with amps.
            self.stacked = true;
        }
        self.lanes = stack::lanes(&traces);
        self.traces = traces;
    }

    fn addr(&self) -> String {
        let host = self.host.trim();
        if crate::usbtmc::is_usb_addr(host) {
            host.to_string()
        } else {
            format!("{}:{}", host, self.port.trim())
        }
    }

    fn selected(&self) -> Vec<String> {
        let supply = self
            .capabilities
            .as_ref()
            .is_some_and(|caps| caps.kind == InstrumentKind::Supply);
        let count = self
            .capabilities
            .as_ref()
            .map(|caps| caps.channel_count)
            .unwrap_or(CHANNELS.len());
        self.config
            .as_ref()
            .map(|config| {
                CHANNELS
                    .iter()
                    .enumerate()
                    .take(count)
                    .filter(|(i, _)| supply || config.channels[*i].enabled)
                    .map(|(_, c)| (*c).to_string())
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
            captured_at: self.captured_at.as_ref(),
        }
    }

    fn export_traces(&mut self, format: crate::export::ExportFormat) {
        let name = crate::export::capture_filename(format.extension(), self.captured_at.as_ref());
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
            .set_file_name(crate::export::capture_filename(
                "png",
                self.captured_at.as_ref(),
            ))
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
        if self.screenshot_out.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn show_measurements(&self, ctx: &egui::Context) {
        if self.is_supply() && supply_capture_count(&self.supply_history) < 2 {
            return;
        }
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

    fn show_supply_phosphor(&self, ui: &mut egui::Ui) {
        let latest = latest_supply_samples(&self.supply_history);
        let bg = egui::Color32::from_rgb(4, 16, 8);
        let glow = egui::Color32::from_rgb(80, 255, 70);
        let dim = egui::Color32::from_rgb(24, 92, 36);
        egui::Frame::new()
            .fill(bg)
            .inner_margin(egui::Margin::same(18))
            .corner_radius(egui::CornerRadius::same(6))
            .show(ui, |ui| {
                if latest.is_empty() {
                    ui.label(
                        egui::RichText::new("FETCH")
                            .monospace()
                            .size(64.0)
                            .color(dim),
                    );
                    ui.label(
                        egui::RichText::new("to read voltage and current")
                            .monospace()
                            .size(18.0)
                            .color(dim),
                    );
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    for sample in &latest {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(&sample.channel)
                                    .monospace()
                                    .size(18.0)
                                    .color(dim),
                            );
                            ui.label(
                                egui::RichText::new(meter_text(sample.volts, "V"))
                                    .monospace()
                                    .size(52.0)
                                    .color(glow),
                            );
                            ui.label(
                                egui::RichText::new(meter_text(sample.amps, "A"))
                                    .monospace()
                                    .size(52.0)
                                    .color(glow),
                            );
                            if let Some(prev) = previous_supply_sample(
                                &self.supply_history,
                                &sample.channel,
                                sample.t_s,
                            ) {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}   {}",
                                        meter_text(prev.volts, "V"),
                                        meter_text(prev.amps, "A")
                                    ))
                                    .monospace()
                                    .size(16.0)
                                    .color(dim),
                                );
                            }
                        });
                        ui.add_space(28.0);
                    }
                });
                let n = supply_capture_count(&self.supply_history);
                let caption = if n < 2 {
                    "one reading · fetch again to plot this session".to_string()
                } else {
                    format!("{n} readings · time is seconds since the first")
                };
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(caption)
                        .monospace()
                        .size(14.0)
                        .color(dim),
                );
            });
        ui.add_space(8.0);
    }

    /// Bench-multimeter readings: the latest fetched values, big and green. A
    /// scanning DAQ answers with one reading per channel, so every trace is
    /// shown rather than only the first.
    fn show_meter_phosphor(&self, ui: &mut egui::Ui) {
        let bg = egui::Color32::from_rgb(4, 16, 8);
        let glow = egui::Color32::from_rgb(80, 255, 70);
        let dim = egui::Color32::from_rgb(24, 92, 36);
        egui::Frame::new()
            .fill(bg)
            .inner_margin(egui::Margin::same(18))
            .corner_radius(egui::CornerRadius::same(6))
            .show(ui, |ui| {
                if self.traces.is_empty() {
                    ui.label(
                        egui::RichText::new("FETCH")
                            .monospace()
                            .size(64.0)
                            .color(dim),
                    );
                    ui.label(
                        egui::RichText::new("to take a reading")
                            .monospace()
                            .size(18.0)
                            .color(dim),
                    );
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    for trace in &self.traces {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(&trace.channel)
                                    .monospace()
                                    .size(18.0)
                                    .color(dim),
                            );
                            let value = trace.points.last().map(|point| point[1]);
                            ui.label(
                                egui::RichText::new(meter_text(
                                    value.unwrap_or(f64::NAN),
                                    &trace.y_unit,
                                ))
                                .monospace()
                                .size(52.0)
                                .color(glow),
                            );
                        });
                        ui.add_space(28.0);
                    }
                });
                if let Some(config) = self.config.as_ref().and_then(|c| c.channels.first()) {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "function {} · range {}",
                            config.wave_type, config.probe_type
                        ))
                        .monospace()
                        .size(14.0)
                        .color(dim),
                    );
                }
            });
        ui.add_space(8.0);
    }

    fn pump(&mut self, now: f64) {
        while let Some(msg) = self.worker.try_recv() {
            match msg {
                Msg::Status(s) => self.status = s,
                Msg::Connected {
                    idn,
                    port,
                    capabilities,
                } => {
                    self.status = format!("Connected: {idn}");
                    self.idn = Some(idn);
                    self.capabilities = Some(*capabilities);
                    self.acquisition_status = None;
                    self.status_poll_in_flight = false;
                    self.last_status_poll = 0.0;
                    self.status_poll_retry_at = 0.0;
                    if self.port != port.to_string() {
                        self.port = port.to_string();
                        self.persist();
                    }
                    self.pending = true;
                    self.clear_supply_session();
                    if self.is_supply() {
                        self.set_traces(Vec::new());
                    }
                    self.worker.send(Cmd::ReadConfig);
                }
                Msg::Disconnected => {
                    let supply = self.is_supply();
                    let keep_session_plot =
                        supply && supply_capture_count(&self.supply_history) >= 2;
                    self.idn = None;
                    self.capabilities = None;
                    self.auto = false;
                    self.pending = false;
                    self.config = None;
                    self.acquisition_status = None;
                    self.status_poll_in_flight = false;
                    self.last_status_poll = 0.0;
                    self.status_poll_retry_at = 0.0;
                    self.status = "Disconnected.".into();
                    self.captured_at = None;
                    self.clear_supply_session();
                    if supply && !keep_session_plot {
                        self.set_traces(vec![demo_trace("CH1")]);
                    }
                }
                Msg::Traces {
                    traces: t,
                    captured_at,
                } => {
                    self.pending = false;
                    self.captured_at = Some(captured_at);
                    if self.is_supply() {
                        let elapsed = match self.supply_t0 {
                            Some(t0) => (now - t0).max(0.0),
                            None => {
                                self.supply_t0 = Some(now);
                                0.0
                            }
                        };
                        append_supply_samples(&mut self.supply_history, &t, elapsed);
                        let n = supply_capture_count(&self.supply_history);
                        self.status = match n {
                            0 => "Fetch returned no voltage or current.".into(),
                            1 => "1 reading. Fetch again to plot this session.".into(),
                            n => format!("{n} readings this session"),
                        };
                        self.set_traces(supply_session_traces(&self.supply_history));
                    } else {
                        let n: usize = t.iter().map(|x| x.points.len()).sum();
                        self.status = format!("{n} samples across {} channel(s)", t.len());
                        self.set_traces(t);
                    }
                }
                Msg::Config {
                    config,
                    capabilities,
                } => {
                    self.pending = false;
                    self.config = Some(*config);
                    self.capabilities = Some(*capabilities);
                    self.status = "Instrument settings synchronized.".into();
                }
                Msg::Applied(status) => {
                    self.status = status;
                    self.pending = true;
                    self.worker.send(Cmd::ReadConfig);
                }
                Msg::AcquisitionStatus(status) => {
                    self.status_poll_in_flight = false;
                    match status {
                        Some(status) => {
                            self.acquisition_status = Some(status.display);
                            if let Some(config) = self.config.as_mut() {
                                config.acquisition.running = status.running;
                            }
                        }
                        None => {
                            self.status_poll_retry_at = now + STATUS_POLL_BACKOFF;
                        }
                    }
                }
                Msg::RawResponse(response) => {
                    self.pending = false;
                    self.raw_response = response;
                    self.status = "SCPI query complete.".into();
                }
                Msg::Error(e) => {
                    self.pending = false;
                    self.scanning = false;
                    self.auto = false;
                    // Reconnecting into a wedged socket server only prolongs it.
                    if e.contains("wedged") {
                        self.retry_at = now + 30.0;
                    }
                    self.status = format!("Error: {e}");
                }
                Msg::ScanDone { found, notes } => {
                    self.scanning = false;
                    self.scan_results = found;
                    if self.scan_results.len() == 1 {
                        let scope = &self.scan_results[0];
                        self.host = scope.host.clone();
                        self.port = scope.port.to_string();
                        self.persist();
                    }
                    let n = self.scan_results.len();
                    let mut status = match n {
                        0 => "No instruments found.".into(),
                        1 => format!("Found {}", self.scan_results[0].summary()),
                        n => format!("Found {n} instruments."),
                    };
                    if !notes.is_empty() {
                        status = format!("{status} {}", notes.join(" "));
                    }
                    self.status = status;
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

        // CLI screenshot mode: give the first frames a chance to paint, then
        // capture the window, write the PNG (collect_screenshot), and close.
        if let Some(path) = self.screenshot_out.clone() {
            ctx.request_repaint_after(Duration::from_millis(100));
            if self.screenshot_requested_at.is_none() {
                if self.frames >= 5 {
                    self.screenshot_requested_at = Some(now);
                    self.pending_png = Some(path);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                }
            } else if now - self.screenshot_requested_at.unwrap_or(now) > 5.0 {
                // The compositor never delivered a Screenshot event; do not
                // hang a headless invocation.
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

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
        if self.pending || self.scanning || self.idn.is_some() || now < self.retry_at {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        if self.auto && !self.pending {
            if now - self.last_fetch >= self.auto_interval {
                self.last_fetch = now;
                self.request_fetch();
            }
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        if self.idn.is_some()
            && !self.pending
            && !self.status_poll_in_flight
            && now >= self.retry_at
            && now >= self.status_poll_retry_at
            && now - self.last_status_poll >= STATUS_POLL_INTERVAL
        {
            self.last_status_poll = now;
            self.status_poll_in_flight = true;
            self.worker.send(Cmd::PollStatus);
        }

        // In macOS fullscreen, clicks in the top ~20 px of the window report the
        // wrong coordinates, so widgets there cannot be hit even though the app
        // keeps rendering. The window has to be created hidden to avoid a white
        // flash on startup, and that is what triggers it; there is no fix
        // upstream yet. Keep the toolbar clear of that band.
        // https://github.com/rust-windowing/winit/issues/4295
        // https://github.com/emilk/egui/pull/7281
        let dead_band = if cfg!(target_os = "macos")
            && ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
        {
            28.0
        } else {
            0.0
        };

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(4.0 + dead_band);
            ui.horizontal_wrapped(|ui| {
                let connected = self.idn.is_some();
                ui.add_enabled_ui(!connected, |ui| {
                    ui.label("Host");
                    ui.add(egui::TextEdit::singleline(&mut self.host).desired_width(180.0));
                    if !crate::usbtmc::is_usb_addr(self.host.trim()) {
                        ui.label("Port");
                        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(50.0));
                    }
                });

                if !connected {
                    let cooling = now < self.retry_at;
                    let label = if cooling {
                        format!("Wait {:.0}s", self.retry_at - now)
                    } else {
                        "Connect".to_string()
                    };
                    if ui
                        .add_enabled(
                            !self.pending && !self.scanning && !cooling,
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        self.persist();
                        self.pending = true;
                        self.worker.send(Cmd::Connect { addr: self.addr() });
                    }
                    if ui
                        .add_enabled(
                            !self.pending && !self.scanning && !cooling,
                            egui::Button::new("Scan"),
                        )
                        .clicked()
                    {
                        self.scanning = true;
                        self.status = "Scanning for instruments…".into();
                        self.worker.send(Cmd::Scan);
                    }
                    if !self.scan_results.is_empty() {
                        let current = if crate::usbtmc::is_usb_addr(self.host.trim()) {
                            self.host.trim().to_string()
                        } else {
                            format!("{}:{}", self.host.trim(), self.port.trim())
                        };
                        let results = self.scan_results.clone();
                        egui::ComboBox::from_id_salt("scan-results")
                            .selected_text(current)
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for scope in &results {
                                    let selected = self.host == scope.host
                                        && (crate::usbtmc::is_usb_addr(&scope.host)
                                            || self.port == scope.port.to_string());
                                    if ui.selectable_label(selected, scope.summary()).clicked() {
                                        self.host = scope.host.clone();
                                        self.port = scope.port.to_string();
                                        self.persist();
                                    }
                                }
                            });
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
                        .on_hover_text("Arm one acquisition, wait, then fetch")
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
                if connected {
                    ui.separator();
                    ui.label(format!(
                        "Acq: {}",
                        self.acquisition_status.as_deref().unwrap_or("…")
                    ))
                    .on_hover_text("Live acquisition / trigger state");
                }

                if ui.button("Demo").clicked() {
                    let demo = self.selected().iter().map(|c| demo_trace(c)).collect();
                    self.set_traces(demo);
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
                if ui
                    .selectable_label(self.stacked, "Stack")
                    .on_hover_text(
                        "One band per channel, each scaled to its own min/max, \
                         so a small signal is as tall as a large one",
                    )
                    .clicked()
                {
                    self.stacked = !self.stacked;
                    // The y range changes completely; the old view would be off-screen.
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
            let supply = self.is_supply();
            if supply {
                self.show_supply_phosphor(ui);
            }
            if self.is_multimeter() {
                self.show_meter_phosphor(ui);
                return;
            }
            if supply && supply_capture_count(&self.supply_history) < 2 {
                return;
            }
            if self.traces.is_empty() {
                return;
            }
            let x_unit = self.traces.first().map_or("s", |t| t.x_unit.as_str());
            let x_axis = if supply {
                "s since first reading"
            } else {
                x_unit
            };
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
            let lanes: &[stack::Lane] = if self.stacked { &self.lanes } else { &[] };
            let names: Vec<String> = self.traces.iter().map(|t| t.channel.clone()).collect();
            let units: Vec<String> = self.traces.iter().map(|t| t.y_unit.clone()).collect();
            let mut plot = Plot::new("mdo")
                .legend(Legend::default())
                .x_axis_label(x_axis)
                .allow_zoom(axes)
                .allow_drag(true)
                .allow_boxed_zoom(true)
                // Manual scroll handling below, so the wheel can zoom per axis.
                .allow_scroll(!self.scroll_zooms);
            if lanes.is_empty() {
                plot = plot.y_axis_label(y_unit);
            } else {
                // Stacked y values are lane positions, not engineering units,
                // so label bands by trace and translate hovered points back.
                plot = plot
                    .y_axis_label("per trace")
                    .y_grid_spacer(|_| {
                        (0..lanes.len())
                            .map(|i| GridMark {
                                value: lanes[i].center,
                                step_size: 1.0,
                            })
                            .collect()
                    })
                    .y_axis_formatter(|mark, _| {
                        match lanes
                            .iter()
                            .position(|l| (l.center - mark.value).abs() < 1e-9)
                        {
                            Some(i) => names[i].clone(),
                            None => String::new(),
                        }
                    })
                    .label_formatter(|name, point| {
                        let t = measure::format_si(point.x, x_unit);
                        let lane = names
                            .iter()
                            .position(|n| n == name)
                            .or_else(|| stack::nearest(lanes, point.y));
                        let Some(i) = lane else {
                            return t;
                        };
                        let v = measure::format_si(lanes[i].value(point.y), &units[i]);
                        if name.is_empty() {
                            format!("{t}\n{v}")
                        } else {
                            format!("{name}\n{t}\n{v}")
                        }
                    });
            }
            plot.show(ui, |plot_ui| {
                // Last frame's bounds; good enough to size this frame's detail.
                let visible = plot_ui.plot_bounds();
                for (i, trace) in self.traces.iter().enumerate() {
                    let mut reduced = plotdata::prepare(
                        &trace.points,
                        visible.min()[0],
                        visible.max()[0],
                        width_px,
                        !do_fit,
                    );
                    drawn += reduced.len();
                    // Decimation runs on volts so every channel keeps its own
                    // peaks; the lane map is affine and preserves their order.
                    if let Some(lane) = lanes.get(i) {
                        for p in reduced.iter_mut() {
                            p[1] = lane.plot_y(p[1]);
                        }
                    }
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
                    secondary = resp.secondary_clicked() || resp.ctx.input(|i| i.modifiers.shift);
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
                    if self
                        .capabilities
                        .as_ref()
                        .map(|c| c.kind.is_scope())
                        .unwrap_or(true)
                        && ui
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
                let Some(capabilities) = self.capabilities.clone() else {
                    ui.label("Instrument capabilities unavailable.");
                    return;
                };
                if self.selected_channel >= capabilities.channel_count {
                    self.selected_channel = 0;
                }
                let channel_names: Vec<String> = CHANNELS
                    .iter()
                    .take(capabilities.channel_count)
                    .map(|value| (*value).to_string())
                    .collect();
                let generator = capabilities.kind == InstrumentKind::Generator;
                let supply = capabilities.kind == InstrumentKind::Supply;
                let multimeter = capabilities.kind == InstrumentKind::Multimeter;
                let spectrum = capabilities.kind == InstrumentKind::Spectrum;
                let scope = capabilities.kind.is_scope();

                let channels_label = if scope {
                    "Channels"
                } else if multimeter {
                    "Function"
                } else if spectrum {
                    "Span"
                } else {
                    "Outputs"
                };
                egui::CollapsingHeader::new(channels_label)
                    .default_open(true)
                    .show(ui, |ui| {
                        if supply && !capabilities.output_pairs.is_empty() {
                            ui.label(
                                "Series stacks voltage. Parallel stacks current. Output 2 follows output 1.",
                            );
                            combo_string(
                                ui,
                                "Pairing",
                                &mut config.output_pair,
                                &capabilities.output_pairs,
                            );
                            if ui
                                .add_enabled(!self.pending, egui::Button::new("Apply pairing"))
                                .clicked()
                            {
                                let section = ConfigSection::OutputPair(config.output_pair.clone());
                                self.config = Some(config.clone());
                                self.send_config(section);
                            }
                        }

                        egui::ComboBox::from_label(if supply { "Output" } else { "Channel" })
                            .selected_text(CHANNELS[self.selected_channel])
                            .show_ui(ui, |ui| {
                                for (index, name) in channel_names.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected_channel, index, name);
                                }
                            });

                        let index = self.selected_channel;
                        let output_pair = config.output_pair.clone();
                        let ch = &mut config.channels[index];
                        if generator {
                            ui.checkbox(&mut ch.enabled, "Output enabled");
                            combo_string(ui, "Wave", &mut ch.wave_type, &capabilities.wave_types);
                            value_row(ui, "Frequency (Hz)", &mut ch.frequency_hz, 10.0);
                            value_row(ui, "Amplitude (Vpp)", &mut ch.scale, 0.01);
                            value_row(ui, "Offset (V)", &mut ch.offset, 0.01);
                            let termination = numeric_choice_label(
                                ch.termination_ohms,
                                &capabilities.terminations,
                            );
                            egui::ComboBox::from_label("Load")
                                .selected_text(termination)
                                .show_ui(ui, |ui| {
                                    for choice in &capabilities.terminations {
                                        ui.selectable_value(
                                            &mut ch.termination_ohms,
                                            choice.value,
                                            &choice.label,
                                        );
                                    }
                                });
                            if ch.termination_ohms < 1000.0 {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    "50 Ω load halves the open-circuit amplitude.",
                                );
                            }
                            if let Some(hint) = &capabilities.channel_hint {
                                ui.small(hint);
                            }
                        } else if supply {
                            let slaved = index > 0
                                && matches!(output_pair.as_str(), "PARALLEL" | "SERIES");
                            if slaved {
                                ui.label(format!(
                                    "Output {} follows output 1 in {} mode.",
                                    index + 1,
                                    output_pair.to_ascii_lowercase()
                                ));
                            }
                            ui.add_enabled_ui(!slaved, |ui| {
                                ui.checkbox(&mut ch.enabled, "Output enabled");
                                value_row(ui, "Voltage (V)", &mut ch.scale, 0.01);
                                value_row(ui, "Current limit (A)", &mut ch.offset, 0.001);
                            });
                            ui.label(format!("Measured: {}", ch.probe_type));
                            if let Some(hint) = &capabilities.channel_hint {
                                ui.small(hint);
                            }
                        } else if multimeter {
                            combo_string(ui, "Function", &mut ch.wave_type, &capabilities.wave_types);
                            ui.horizontal(|ui| {
                                ui.label("Reading");
                                ui.strong(format!(
                                    "{} {}",
                                    meter_text(ch.scale, &ch.probe_type),
                                    ch.probe_type
                                ));
                            });
                            if let Some(hint) = &capabilities.channel_hint {
                                ui.small(hint);
                            }
                        } else if spectrum {
                            value_row(ui, "Center (Hz)", &mut config.horizontal.position, 1e6);
                            value_row(ui, "Span (Hz)", &mut config.horizontal.scale, 1e5);
                            if let Some(hint) = &capabilities.horizontal_hint {
                                ui.small(hint);
                            }
                            if ui
                                .add_enabled(!self.pending, egui::Button::new("Apply span"))
                                .clicked()
                            {
                                let section = ConfigSection::Horizontal(config.horizontal.clone());
                                if let Some(stored) = self.config.as_mut() {
                                    stored.horizontal = config.horizontal.clone();
                                }
                                self.send_config(section);
                            }
                        } else {
                            ui.checkbox(&mut ch.enabled, "Displayed / fetched");
                            value_row(ui, "Scale (V/div)", &mut ch.scale, 0.01);
                            value_row(ui, "Position (div)", &mut ch.position, 0.1);
                            value_row(ui, "Offset (V)", &mut ch.offset, 0.01);

                            egui::ComboBox::from_label("Coupling")
                                .selected_text(&ch.coupling)
                                .show_ui(ui, |ui| {
                                    for value in &capabilities.channel_couplings {
                                        ui.selectable_value(&mut ch.coupling, value.clone(), value);
                                    }
                                });

                            let termination = numeric_choice_label(
                                ch.termination_ohms,
                                &capabilities.terminations,
                            );
                            if capabilities.termination_writable {
                                egui::ComboBox::from_label("Input")
                                    .selected_text(termination)
                                    .show_ui(ui, |ui| {
                                        for choice in &capabilities.terminations {
                                            ui.selectable_value(
                                                &mut ch.termination_ohms,
                                                choice.value,
                                                &choice.label,
                                            );
                                        }
                                    });
                            } else {
                                ui.horizontal(|ui| {
                                    ui.label("Input");
                                    ui.add_enabled(false, egui::Label::new(termination));
                                });
                            }
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
                                .selected_text(numeric_choice_label(
                                    ch.bandwidth_hz,
                                    &capabilities.bandwidths,
                                ))
                                .show_ui(ui, |ui| {
                                    for choice in &capabilities.bandwidths {
                                        ui.selectable_value(
                                            &mut ch.bandwidth_hz,
                                            choice.value,
                                            &choice.label,
                                        );
                                    }
                                });
                            if let Some(hint) = &capabilities.channel_hint {
                                ui.small(hint);
                            }
                        }

                        if !spectrum
                            && ui
                                .add_enabled(
                                    !self.pending && !(supply && index > 0 && matches!(output_pair.as_str(), "PARALLEL" | "SERIES")),
                                    egui::Button::new("Apply channel"),
                                )
                                .clicked()
                        {
                            let section = ConfigSection::Channel(index, ch.clone());
                            self.config = Some(config.clone());
                            self.send_config(section);
                        }
                    });

                if scope {
                    egui::CollapsingHeader::new("Horizontal")
                        .default_open(true)
                        .show(ui, |ui| {
                            let h = &mut config.horizontal;
                            value_row(ui, "Time/div (s)", &mut h.scale, 1e-6);
                            value_row(ui, "Position (%)", &mut h.position, 1.0);
                            egui::ComboBox::from_label("Record length")
                                .selected_text(h.record_length.to_string())
                                .show_ui(ui, |ui| {
                                    for &value in &capabilities.record_lengths {
                                        ui.selectable_value(
                                            &mut h.record_length,
                                            value,
                                            format_count(value),
                                        );
                                    }
                                });
                            if let Some(hint) = &capabilities.horizontal_hint {
                                ui.small(hint);
                            }
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
                            combo_string(ui, "Mode", &mut t.mode, &capabilities.trigger_modes);
                            combo_string(ui, "Source", &mut t.source, &channel_names);
                            combo_string(ui, "Slope", &mut t.slope, &capabilities.trigger_slopes);
                            combo_string(
                                ui,
                                "Coupling",
                                &mut t.coupling,
                                &capabilities.trigger_couplings,
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
                            combo_string(ui, "Mode", &mut a.mode, &capabilities.acquisition_modes);
                            combo_string(
                                ui,
                                "Stop after",
                                &mut a.stop_after,
                                &capabilities.stop_after,
                            );
                            ui.checkbox(&mut a.running, "Running");
                            if let Some(hint) = &capabilities.acquisition_hint {
                                ui.small(hint);
                            }
                            if ui
                                .add_enabled(!self.pending, egui::Button::new("Apply acquisition"))
                                .clicked()
                            {
                                let section = ConfigSection::Acquisition(a.clone());
                                self.config = Some(config.clone());
                                self.send_config(section);
                            }
                        });
                }

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

fn meter_text(value: f64, unit: &str) -> String {
    if value.is_finite() {
        measure::format_si(value, unit)
    } else {
        format!("— {unit}")
    }
}

fn split_supply_quantity(name: &str) -> Option<(&str, &str)> {
    if let Some(channel) = name.strip_suffix(" Voltage") {
        Some((channel, "V"))
    } else {
        name.strip_suffix(" Current").map(|channel| (channel, "A"))
    }
}

fn next_supply_time(history: &[SupplySample], elapsed: f64) -> f64 {
    match history.last() {
        Some(sample) if elapsed <= sample.t_s => sample.t_s + 1e-3,
        _ => elapsed.max(0.0),
    }
}

fn append_supply_samples(history: &mut Vec<SupplySample>, traces: &[ChannelTrace], elapsed: f64) {
    let t_s = next_supply_time(history, elapsed);
    let mut order = Vec::new();
    let mut volts = std::collections::BTreeMap::new();
    let mut amps = std::collections::BTreeMap::new();
    for trace in traces {
        let Some((channel, kind)) = split_supply_quantity(&trace.channel) else {
            continue;
        };
        let Some(value) = trace.points.last().map(|point| point[1]) else {
            continue;
        };
        if !volts.contains_key(channel) && !amps.contains_key(channel) {
            order.push(channel.to_string());
        }
        match kind {
            "V" => {
                volts.insert(channel.to_string(), value);
            }
            "A" => {
                amps.insert(channel.to_string(), value);
            }
            _ => {}
        }
    }
    for channel in order {
        history.push(SupplySample {
            t_s,
            channel: channel.clone(),
            volts: volts.get(&channel).copied().unwrap_or(f64::NAN),
            amps: amps.get(&channel).copied().unwrap_or(f64::NAN),
        });
    }
}

fn supply_capture_count(history: &[SupplySample]) -> usize {
    let mut count = 0usize;
    let mut last = f64::NAN;
    for sample in history {
        if last.is_nan() || (sample.t_s - last).abs() > 1e-9 {
            count += 1;
            last = sample.t_s;
        }
    }
    count
}

fn latest_supply_samples(history: &[SupplySample]) -> Vec<&SupplySample> {
    let Some(last) = history.last() else {
        return Vec::new();
    };
    history
        .iter()
        .filter(|sample| (sample.t_s - last.t_s).abs() <= 1e-9)
        .collect()
}

fn previous_supply_sample<'a>(
    history: &'a [SupplySample],
    channel: &str,
    latest_t: f64,
) -> Option<&'a SupplySample> {
    history
        .iter()
        .rev()
        .find(|sample| sample.channel == channel && (sample.t_s - latest_t).abs() > 1e-9)
}

fn supply_session_traces(history: &[SupplySample]) -> Vec<ChannelTrace> {
    let mut channels = Vec::new();
    for sample in history {
        if !channels.iter().any(|name: &String| name == &sample.channel) {
            channels.push(sample.channel.clone());
        }
    }
    let mut traces = Vec::new();
    for channel in channels {
        let volts: Vec<[f64; 2]> = history
            .iter()
            .filter(|sample| sample.channel == channel && sample.volts.is_finite())
            .map(|sample| [sample.t_s, sample.volts])
            .collect();
        let amps: Vec<[f64; 2]> = history
            .iter()
            .filter(|sample| sample.channel == channel && sample.amps.is_finite())
            .map(|sample| [sample.t_s, sample.amps])
            .collect();
        if !volts.is_empty() {
            traces.push(ChannelTrace {
                channel: format!("{channel} Voltage"),
                x_unit: "s".into(),
                y_unit: "V".into(),
                points: volts,
            });
        }
        if !amps.is_empty() {
            traces.push(ChannelTrace {
                channel: format!("{channel} Current"),
                x_unit: "s".into(),
                y_unit: "A".into(),
                points: amps,
            });
        }
    }
    traces
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

fn combo_string(ui: &mut egui::Ui, label: &str, current: &mut String, values: &[String]) {
    egui::ComboBox::from_label(label)
        .selected_text(current.as_str())
        .show_ui(ui, |ui| {
            for value in values {
                ui.selectable_value(current, value.clone(), value);
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

fn numeric_choice_label(value: f64, choices: &[ValueChoice]) -> String {
    choices
        .iter()
        .find(|choice| {
            let scale = value.abs().max(choice.value.abs()).max(1.0);
            (value - choice.value).abs() <= scale * 1e-9
        })
        .map(|choice| choice.label.clone())
        .unwrap_or_else(|| value.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(channel: &str, volts: f64, amps: f64) -> Vec<ChannelTrace> {
        vec![
            ChannelTrace {
                channel: format!("{channel} Voltage"),
                x_unit: "s".into(),
                y_unit: "V".into(),
                points: vec![[0.0, volts]],
            },
            ChannelTrace {
                channel: format!("{channel} Current"),
                x_unit: "s".into(),
                y_unit: "A".into(),
                points: vec![[0.0, amps]],
            },
        ]
    }

    #[test]
    fn one_supply_fetch_is_a_single_sample() {
        let mut history = Vec::new();
        append_supply_samples(&mut history, &reading("CH1", 12.0, 0.25), 0.0);
        assert_eq!(supply_capture_count(&history), 1);
        let traces = supply_session_traces(&history);
        assert_eq!(traces.len(), 2);
        assert!(traces.iter().all(|trace| trace.points.len() == 1));
        assert_eq!(traces[0].points, vec![[0.0, 12.0]]);
        assert_eq!(traces[1].points, vec![[0.0, 0.25]]);
        assert_eq!(traces[1].y_unit, "A");
    }

    #[test]
    fn later_fetches_are_plotted_on_session_time() {
        let mut history = Vec::new();
        let mut first = reading("CH1", 1.0, 0.1);
        first.extend(reading("CH2", 2.0, 0.2));
        append_supply_samples(&mut history, &first, 0.0);
        append_supply_samples(&mut history, &reading("CH1", 1.5, 0.3), 2.5);
        assert_eq!(supply_capture_count(&history), 2);
        let traces = supply_session_traces(&history);
        let ch1_v = traces
            .iter()
            .find(|trace| trace.channel == "CH1 Voltage")
            .unwrap();
        assert_eq!(ch1_v.points, vec![[0.0, 1.0], [2.5, 1.5]]);
        let ch2_v = traces
            .iter()
            .find(|trace| trace.channel == "CH2 Voltage")
            .unwrap();
        assert_eq!(ch2_v.points, vec![[0.0, 2.0]]);
        assert_eq!(latest_supply_samples(&history).len(), 1);
        assert_eq!(latest_supply_samples(&history)[0].channel, "CH1");
    }

    #[test]
    fn identical_clock_readings_stay_distinct() {
        let mut history = Vec::new();
        append_supply_samples(&mut history, &reading("CH1", 1.0, 0.1), 0.0);
        append_supply_samples(&mut history, &reading("CH1", 1.1, 0.2), 0.0);
        assert_eq!(supply_capture_count(&history), 2);
        assert!(history[1].t_s > history[0].t_s);
    }
}
