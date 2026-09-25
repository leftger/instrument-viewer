mod afg;
mod app;
mod backend;
mod cli;
mod config;
mod daq4000a;
mod discover;
mod dm3058;
mod ds1000z;
mod dsa800;
mod export;
mod hdm3000;
mod hrdo2000;
mod keysight;
mod measure;
mod plotdata;
mod prefs;
mod profile;
mod registry;
mod rigol;
mod scpi;
mod sds;
mod siglent;
mod siglent_ssa;
mod stack;
mod tek;
mod timestamp;
mod transport;
mod usbtmc;
mod waveform;
mod worker;

use clap::Parser;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cli::Cli::parse();
    match cli.command.as_ref() {
        None | Some(cli::Command::Gui) => run_gui(cli.host, cli.port, cli.screenshot)?,
        Some(cli::Command::Discover) => cli::discover()?,
        Some(cli::Command::Selftest {
            cycles,
            interval,
            reconnect,
        }) => cli::selftest(&cli, *cycles, *interval, *reconnect)?,
        Some(command) => cli::run(&cli, command)?,
    }
    Ok(())
}

fn run_gui(host: String, port: u16, screenshot: Option<std::path::PathBuf>) -> eframe::Result<()> {
    let prefs = prefs::load();
    let host = if prefs::argv_has("--host") {
        host
    } else {
        prefs.host.clone()
    };
    let port = if prefs::argv_has("--port") {
        port
    } else {
        prefs.port.parse().unwrap_or(port)
    };
    let options = eframe::NativeOptions {
        // Metal rather than eframe's default OpenGL backend. Apple deprecated
        // OpenGL, and on the green-button fullscreen transition the glow path
        // stops driving the event loop: the window goes to its own Space and
        // never paints or handles input again.
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_title("Instrument viewer"),
        ..Default::default()
    };
    eframe::run_native(
        "Instrument viewer",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::ViewerApp::new(
                cc, host, port, prefs, screenshot,
            )))
        }),
    )
}
