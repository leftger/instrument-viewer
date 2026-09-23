mod acquire;
mod app;
mod cli;
mod config;
mod export;
mod measure;
mod plotdata;
mod prefs;
mod scpi;
mod waveform;
mod worker;

use clap::Parser;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cli::Cli::parse();
    match cli.command.as_ref() {
        None | Some(cli::Command::Gui) => run_gui(cli.host, cli.port)?,
        Some(cli::Command::Selftest {
            cycles,
            interval,
            reconnect,
        }) => cli::selftest(&cli, *cycles, *interval, *reconnect)?,
        Some(command) => cli::run(&cli, command)?,
    }
    Ok(())
}

fn run_gui(host: String, port: u16) -> eframe::Result<()> {
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
            .with_title("MDO viewer"),
        ..Default::default()
    };
    eframe::run_native(
        "MDO viewer",
        options,
        Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, host, port, prefs)))),
    )
}
