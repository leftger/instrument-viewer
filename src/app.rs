use std::time::Duration;

use eframe::egui;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::config::{ConfigSection, InstrumentConfig};
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
    last_fetch: f64,
    /// UI clock time before which reconnecting is pointless, after a wedge.
    retry_at: f64,
    raw_command: String,
    raw_response: String,
    worker: Worker,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, host: String, port: u16) -> Self {
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
            auto_interval: 2.0,
            pending: false,
            last_fetch: 0.0,
            retry_at: 0.0,
            raw_command: "*IDN?".into(),
            raw_response: String::new(),
            worker,
        }
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
        self.pump(now);

        if now < self.retry_at {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        if self.auto && !self.pending {
            if now - self.last_fetch >= self.auto_interval {
                self.last_fetch = now;
                self.request_fetch();
            }
            // Only needs to wake often enough to notice the interval elapsing.
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
                    // Must stay disabled while a connect is in flight. Queued
                    // duplicate connects churn sockets, which wedges the scope.
                    if ui
                        .add_enabled(!self.pending && !cooling, egui::Button::new(label))
                        .clicked()
                    {
                        self.pending = true;
                        self.worker.send(Cmd::Connect { addr: self.addr() });
                    }
                } else if ui
                    .add_enabled(!self.pending, egui::Button::new("Disconnect"))
                    .clicked()
                {
                    self.pending = true;
                    self.worker.send(Cmd::Disconnect);
                }

                ui.add_enabled_ui(connected && !self.pending, |ui| {
                    if ui.button("Fetch").clicked() {
                        self.request_fetch();
                    }
                });
                ui.add_enabled_ui(connected, |ui| {
                    ui.checkbox(&mut self.auto, "Auto");
                    ui.add(
                        egui::DragValue::new(&mut self.auto_interval)
                            .speed(0.1)
                            .range(0.5..=30.0)
                            .suffix(" s"),
                    )
                    .on_hover_text("Seconds between automatic captures");
                });

                if ui.button("Demo").clicked() {
                    self.traces = self.selected().iter().map(|c| demo_trace(c)).collect();
                    self.status = "Demo waveform (no instrument).".into();
                }

                if self.pending {
                    ui.spinner();
                }
            });
            ui.label(&self.status);
            ui.add_space(4.0);
        });

        self.show_controls(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            let x_unit = self.traces.first().map_or("s", |t| t.x_unit.as_str());
            let y_unit = self.traces.first().map_or("V", |t| t.y_unit.as_str());
            Plot::new("mdo")
                .legend(Legend::default())
                .x_axis_label(x_unit)
                .y_axis_label(y_unit)
                .show(ui, |plot_ui| {
                    for trace in &self.traces {
                        let pts = PlotPoints::from_iter(trace.points.iter().map(|p| [p[0], p[1]]));
                        plot_ui.line(Line::new(trace.channel.clone(), pts));
                    }
                });
        });
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
