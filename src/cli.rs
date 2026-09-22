use std::error::Error;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::{apply_section, read_config, ConfigSection};
use crate::export::{self, ExportFormat};
use crate::scpi::ScpiSession;
use crate::waveform::fetch_channel;

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    /// Oscilloscope hostname or IP address.
    #[arg(long, default_value = "169.254.6.252", global = true)]
    pub host: String,
    /// Raw SCPI socket-server port.
    #[arg(long, default_value_t = 4000, global = true)]
    pub port: u16,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Launch the graphical viewer (the default with no subcommand).
    Gui,
    /// Print the current channel, horizontal, trigger and acquisition settings.
    Get,
    /// Send or query an arbitrary SCPI command.
    Scpi {
        #[command(subcommand)]
        action: ScpiAction,
    },
    /// Configure one analog channel.
    Channel(ChannelArgs),
    /// Configure timebase, horizontal position and record length.
    Horizontal(HorizontalArgs),
    /// Configure an edge trigger.
    Trigger(TriggerArgs),
    /// Configure acquisition mode and run state.
    Acquisition(AcquisitionArgs),
    /// Fetch the current waveform(s) and write CSV or JSON.
    Export {
        /// csv or json (taken from --out suffix when omitted).
        #[arg(long, value_enum)]
        format: Option<ExportKind>,
        /// Destination file. Writes to stdout if omitted.
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
        /// Channels to capture. Defaults to those enabled on the instrument.
        #[arg(long, value_delimiter = ',')]
        channels: Vec<String>,
    },
    /// Run the scope's built-in autoset.
    Autoset,
    /// Drive the GUI's worker thread headlessly to reproduce connect problems.
    Selftest {
        /// Number of read-config/fetch cycles to run.
        #[arg(long, default_value_t = 3)]
        cycles: usize,
        /// Seconds to wait between cycles. Back-to-back cycles wedge the
        /// instrument, so this matches the GUI's unhurried default.
        #[arg(long, default_value_t = 2.0)]
        interval: f64,
        /// Reconnect before every cycle. This is known to wedge the instrument
        /// and exists only to demonstrate that failure mode.
        #[arg(long)]
        reconnect: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ExportKind {
    Csv,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum ScpiAction {
    Query { command: String },
    Write { command: String },
}

#[derive(Args, Debug)]
pub struct ChannelArgs {
    #[arg(value_parser = parse_channel_number)]
    channel: usize,
    #[arg(long)]
    enabled: Option<bool>,
    /// Vertical scale, in volts/div or with an SI suffix (for example 500mV).
    #[arg(long, value_parser = parse_si)]
    scale: Option<f64>,
    #[arg(long)]
    position: Option<f64>,
    /// Vertical offset, in volts or with an SI suffix.
    #[arg(long, value_parser = parse_si)]
    offset: Option<f64>,
    #[arg(long, value_enum)]
    coupling: Option<ChannelCoupling>,
    #[arg(long, value_enum)]
    termination: Option<Termination>,
    /// Passive-probe attenuation, for example 1, 10 or 100.
    #[arg(long)]
    probe: Option<f64>,
    /// Bandwidth in Hz/SI form (20MHz), or `full`.
    #[arg(long)]
    bandwidth: Option<String>,
}

#[derive(Args, Debug)]
pub struct HorizontalArgs {
    /// Seconds/div or an SI duration (for example 2us).
    #[arg(long, value_parser = parse_si)]
    scale: Option<f64>,
    #[arg(long)]
    position: Option<f64>,
    /// 1k, 10k, 100k, 1M, 5M or 10M.
    #[arg(long, value_parser = parse_count)]
    record_length: Option<u64>,
}

#[derive(Args, Debug)]
pub struct TriggerArgs {
    #[arg(long, value_enum)]
    mode: Option<TriggerMode>,
    #[arg(long, value_enum)]
    source: Option<Channel>,
    #[arg(long, value_enum)]
    slope: Option<Slope>,
    #[arg(long, value_enum)]
    coupling: Option<TriggerCoupling>,
    /// Trigger voltage or an SI voltage (for example 250mV).
    #[arg(long, value_parser = parse_si)]
    level: Option<f64>,
}

#[derive(Args, Debug)]
pub struct AcquisitionArgs {
    #[arg(long, value_enum)]
    mode: Option<AcquisitionMode>,
    #[arg(long, value_enum)]
    stop_after: Option<StopAfter>,
    #[arg(long)]
    running: Option<bool>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum ChannelCoupling {
    Dc,
    Ac,
    DcReject,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Termination {
    Fifty,
    OneMeg,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum TriggerMode {
    Auto,
    Normal,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Channel {
    Ch1,
    Ch2,
    Ch3,
    Ch4,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Slope {
    Rise,
    Fall,
    Either,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum TriggerCoupling {
    Dc,
    Ac,
    HfReject,
    LfReject,
    NoiseReject,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum AcquisitionMode {
    Sample,
    PeakDetect,
    HiRes,
    Average,
    Envelope,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum StopAfter {
    RunStop,
    Sequence,
}

pub fn run(cli: &Cli, command: &Command) -> Result<(), Box<dyn Error>> {
    let addr = format!("{}:{}", cli.host, cli.port);
    let mut session = ScpiSession::connect(&addr, Duration::from_secs(6))?;
    let mut idn = session.query("*IDN?")?;
    if !idn.contains(',') {
        let _ = session.resync();
        idn = session.query("*IDN?")?;
    }

    match command {
        Command::Gui => unreachable!("GUI is dispatched by main"),
        Command::Get => {
            let c = read_config(&mut session)?;
            println!("{idn}");
            for (i, ch) in c.channels.iter().enumerate() {
                println!(
                    "CH{} enabled={} scale={} V/div position={} div offset={} V coupling={} input={} ohm probe={}x ({}) bandwidth={} Hz",
                    i + 1,
                    ch.enabled,
                    ch.scale,
                    ch.position,
                    ch.offset,
                    ch.coupling,
                    ch.termination_ohms,
                    1.0 / ch.probe_gain,
                    ch.probe_type,
                    ch.bandwidth_hz
                );
            }
            println!(
                "Horizontal scale={} s/div position={}% record_length={}",
                c.horizontal.scale, c.horizontal.position, c.horizontal.record_length
            );
            println!(
                "Trigger mode={} source={} slope={} coupling={} level={} V",
                c.trigger.mode,
                c.trigger.source,
                c.trigger.slope,
                c.trigger.coupling,
                c.trigger.level
            );
            println!(
                "Acquisition mode={} stop_after={} running={}",
                c.acquisition.mode, c.acquisition.stop_after, c.acquisition.running
            );
        }
        Command::Export {
            format,
            out,
            channels,
        } => {
            let format = resolve_format(*format, out.as_deref())?;
            let channels = if channels.is_empty() {
                let config = read_config(&mut session)?;
                let enabled: Vec<String> = (0..4)
                    .filter(|&i| config.channels[i].enabled)
                    .map(|i| format!("CH{}", i + 1))
                    .collect();
                if enabled.is_empty() {
                    vec!["CH1".into()]
                } else {
                    enabled
                }
            } else {
                channels
                    .iter()
                    .map(|c| c.trim().to_ascii_uppercase())
                    .collect()
            };
            let mut traces = Vec::new();
            for ch in &channels {
                traces.push(fetch_channel(&mut session, ch)?);
            }
            let body = export::render(&traces, Some(&idn), format);
            match out {
                Some(path) => {
                    std::fs::write(path, body)?;
                    println!(
                        "wrote {} ({:?}, {} channel(s))",
                        path.display(),
                        format,
                        traces.len()
                    );
                }
                None => print!("{body}"),
            }
        }
        Command::Scpi { action } => match action {
            ScpiAction::Query { command } => println!("{}", session.query(command)?),
            ScpiAction::Write { command } => {
                session.write(command)?;
                println!("{}", session.query("*OPC?")?);
            }
        },
        Command::Channel(args) => {
            let mut c = read_config(&mut session)?;
            let ch = &mut c.channels[args.channel - 1];
            if let Some(v) = args.enabled {
                ch.enabled = v;
            }
            if let Some(v) = args.scale {
                ch.scale = v;
            }
            if let Some(v) = args.position {
                ch.position = v;
            }
            if let Some(v) = args.offset {
                ch.offset = v;
            }
            if let Some(v) = args.coupling {
                ch.coupling = channel_coupling(v).into();
            }
            if let Some(v) = args.termination {
                ch.termination_ohms = match v {
                    Termination::Fifty => 50.0,
                    Termination::OneMeg => 1e6,
                };
            }
            if let Some(v) = args.probe {
                if v <= 0.0 {
                    return Err("probe attenuation must be positive".into());
                }
                ch.probe_gain = 1.0 / v;
            }
            if let Some(v) = &args.bandwidth {
                ch.bandwidth_hz = if v.eq_ignore_ascii_case("full") {
                    200e6
                } else {
                    parse_si(v)?
                };
            }
            apply_section(
                &mut session,
                &ConfigSection::Channel(args.channel - 1, ch.clone()),
            )?;
            println!("CH{} settings applied", args.channel);
        }
        Command::Horizontal(args) => {
            let mut c = read_config(&mut session)?;
            if let Some(v) = args.scale {
                c.horizontal.scale = v;
            }
            if let Some(v) = args.position {
                c.horizontal.position = v;
            }
            if let Some(v) = args.record_length {
                c.horizontal.record_length = v;
            }
            apply_section(&mut session, &ConfigSection::Horizontal(c.horizontal))?;
            println!("horizontal settings applied");
        }
        Command::Trigger(args) => {
            let mut c = read_config(&mut session)?;
            if let Some(v) = args.mode {
                c.trigger.mode = match v {
                    TriggerMode::Auto => "AUTO",
                    TriggerMode::Normal => "NORMAL",
                }
                .into();
            }
            if let Some(v) = args.source {
                c.trigger.source = channel(v).into();
            }
            if let Some(v) = args.slope {
                c.trigger.slope = match v {
                    Slope::Rise => "RISE",
                    Slope::Fall => "FALL",
                    Slope::Either => "EITHER",
                }
                .into();
            }
            if let Some(v) = args.coupling {
                c.trigger.coupling = trigger_coupling(v).into();
            }
            if let Some(v) = args.level {
                c.trigger.level = v;
            }
            apply_section(&mut session, &ConfigSection::Trigger(c.trigger))?;
            println!("edge-trigger settings applied");
        }
        Command::Acquisition(args) => {
            let mut c = read_config(&mut session)?;
            if let Some(v) = args.mode {
                c.acquisition.mode = acquisition_mode(v).into();
            }
            if let Some(v) = args.stop_after {
                c.acquisition.stop_after = match v {
                    StopAfter::RunStop => "RUNSTOP",
                    StopAfter::Sequence => "SEQUENCE",
                }
                .into();
            }
            if let Some(v) = args.running {
                c.acquisition.running = v;
            }
            apply_section(&mut session, &ConfigSection::Acquisition(c.acquisition))?;
            println!("acquisition settings applied");
        }
        Command::Autoset => {
            session.write("AUTOSET EXECUTE")?;
            println!("autoset started");
        }
        Command::Selftest { .. } => unreachable!("selftest is dispatched by main"),
    }
    Ok(())
}

fn resolve_format(
    format: Option<ExportKind>,
    out: Option<&std::path::Path>,
) -> Result<ExportFormat, Box<dyn Error>> {
    if let Some(kind) = format {
        return Ok(match kind {
            ExportKind::Csv => ExportFormat::Csv,
            ExportKind::Json => ExportFormat::Json,
        });
    }
    match out.and_then(|p| p.extension()?.to_str()) {
        Some("json") => Ok(ExportFormat::Json),
        Some("csv") | None => Ok(ExportFormat::Csv),
        Some(other) => {
            Err(format!("unknown export format '.{other}'; use --format csv|json").into())
        }
    }
}

/// Exercise the same worker the GUI drives, so failures can be reproduced
/// without clicking buttons.
pub fn selftest(
    cli: &Cli,
    cycles: usize,
    interval: f64,
    reconnect: bool,
) -> Result<(), Box<dyn Error>> {
    use crate::worker::{Cmd, Msg, Worker};
    use std::time::Instant;

    let addr = format!("{}:{}", cli.host, cli.port);
    let worker = Worker::spawn(|| {});
    let mut failures = 0;

    /// Pump worker messages until the cycle finishes or times out.
    fn await_cycle(
        worker: &crate::worker::Worker,
        started: Instant,
        mut want_connect: bool,
    ) -> Result<(), String> {
        let mut configured = false;
        let mut fetched = false;
        let deadline = Instant::now() + Duration::from_secs(40);

        while Instant::now() < deadline && !fetched {
            let Some(msg) = worker.try_recv() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            match msg {
                Msg::Status(s) => println!("  [{:>8.0?}] {s}", started.elapsed()),
                Msg::Connected(idn) => {
                    want_connect = false;
                    println!("  [{:>8.0?}] connected: {idn}", started.elapsed());
                    worker.send(Cmd::ReadConfig);
                }
                Msg::Config(_) => {
                    configured = true;
                    worker.send(Cmd::Fetch {
                        channels: vec!["CH1".into()],
                    });
                }
                Msg::Traces(t) => {
                    fetched = true;
                    let n: usize = t.iter().map(|x| x.points.len()).sum();
                    println!("  [{:>8.0?}] {n} samples", started.elapsed());
                }
                Msg::Applied(_) | Msg::RawResponse(_) | Msg::Disconnected => {}
                Msg::Error(e) => return Err(e),
            }
        }

        if want_connect {
            return Err("never connected".into());
        }
        if !configured {
            return Err("config never read".into());
        }
        if !fetched {
            return Err("fetch timed out".into());
        }
        Ok(())
    }

    if !reconnect {
        let started = Instant::now();
        worker.send(Cmd::Connect { addr: addr.clone() });
        if let Err(e) = await_cycle(&worker, started, true) {
            worker.send(Cmd::Disconnect);
            return Err(format!("initial connect failed: {e}").into());
        }
        println!("cycle 1: ok in {:?}", started.elapsed());
    }

    let gap = Duration::from_secs_f64(interval.max(0.0));
    let first = if reconnect { 1 } else { 2 };
    let mut consecutive = 0;
    for cycle in first..=cycles {
        std::thread::sleep(gap);
        let started = Instant::now();
        if reconnect {
            worker.send(Cmd::Connect { addr: addr.clone() });
        } else {
            worker.send(Cmd::ReadConfig);
        }

        match await_cycle(&worker, started, reconnect) {
            Ok(()) => {
                consecutive = 0;
                println!("cycle {cycle}: ok in {:?}", started.elapsed());
            }
            Err(e) => {
                failures += 1;
                consecutive += 1;
                println!("cycle {cycle}: FAILED in {:?} — {e}", started.elapsed());
                // Once it starts timing out it needs quiet, not more traffic.
                if consecutive >= 2 {
                    println!("stopping early: instrument is not responding");
                    break;
                }
            }
        }

        if reconnect {
            worker.send(Cmd::Disconnect);
            std::thread::sleep(Duration::from_millis(250));
            while worker.try_recv().is_some() {}
        }
    }

    worker.send(Cmd::Disconnect);
    std::thread::sleep(Duration::from_millis(300));

    if failures > 0 {
        return Err(format!("{failures} failure(s) across {cycles} cycle(s)").into());
    }
    println!("all {cycles} cycle(s) passed");
    Ok(())
}

fn channel_coupling(value: ChannelCoupling) -> &'static str {
    match value {
        ChannelCoupling::Dc => "DC",
        ChannelCoupling::Ac => "AC",
        ChannelCoupling::DcReject => "DCREJECT",
    }
}

fn channel(value: Channel) -> &'static str {
    match value {
        Channel::Ch1 => "CH1",
        Channel::Ch2 => "CH2",
        Channel::Ch3 => "CH3",
        Channel::Ch4 => "CH4",
    }
}

fn trigger_coupling(value: TriggerCoupling) -> &'static str {
    match value {
        TriggerCoupling::Dc => "DC",
        TriggerCoupling::Ac => "AC",
        TriggerCoupling::HfReject => "HFREJ",
        TriggerCoupling::LfReject => "LFREJ",
        TriggerCoupling::NoiseReject => "NOISEREJ",
    }
}

fn acquisition_mode(value: AcquisitionMode) -> &'static str {
    match value {
        AcquisitionMode::Sample => "SAMPLE",
        AcquisitionMode::PeakDetect => "PEAKDETECT",
        AcquisitionMode::HiRes => "HIRES",
        AcquisitionMode::Average => "AVERAGE",
        AcquisitionMode::Envelope => "ENVELOPE",
    }
}

fn parse_si(input: &str) -> Result<f64, String> {
    let s = input.trim();
    if let Ok(value) = s.parse::<f64>() {
        return Ok(value);
    }

    // Longest suffix first so `MHz` is not mistaken for `Hz`.
    let suffixes = [
        ("GHz", 1e9),
        ("MHz", 1e6),
        ("kHz", 1e3),
        ("Hz", 1.0),
        ("mV", 1e-3),
        ("uV", 1e-6),
        ("µV", 1e-6),
        ("V", 1.0),
        ("ms", 1e-3),
        ("us", 1e-6),
        ("µs", 1e-6),
        ("ns", 1e-9),
        ("ps", 1e-12),
        ("s", 1.0),
    ];
    for (suffix, factor) in suffixes {
        if let Some(number) = s.strip_suffix(suffix) {
            return number
                .trim()
                .parse::<f64>()
                .map(|v| v * factor)
                .map_err(|_| format!("invalid value {input:?}"));
        }
    }
    Err(format!("invalid SI value {input:?}"))
}

fn parse_count(input: &str) -> Result<u64, String> {
    if let Ok(value) = input.parse() {
        return Ok(value);
    }
    let lower = input.trim().to_ascii_lowercase();
    let (number, factor) = if let Some(n) = lower.strip_suffix('k') {
        (n, 1_000)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 1_000_000)
    } else {
        return Err(format!("invalid record length {input:?}"));
    };
    number
        .parse::<u64>()
        .map(|v| v * factor)
        .map_err(|_| format!("invalid record length {input:?}"))
}

fn parse_channel_number(input: &str) -> Result<usize, String> {
    match input.parse::<usize>() {
        Ok(value @ 1..=4) => Ok(value),
        _ => Err("channel must be 1, 2, 3, or 4".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_si_values() {
        assert_eq!(parse_si("500mV").unwrap(), 0.5);
        assert_eq!(parse_si("2us").unwrap(), 2e-6);
        assert_eq!(parse_si("20MHz").unwrap(), 20e6);
    }

    #[test]
    fn parses_record_lengths() {
        assert_eq!(parse_count("10k").unwrap(), 10_000);
        assert_eq!(parse_count("5M").unwrap(), 5_000_000);
    }
}
