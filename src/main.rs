mod app;
mod cli;
mod config;
mod export;
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
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_title("MDO viewer"),
        ..Default::default()
    };
    eframe::run_native(
        "MDO viewer",
        options,
        Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, host, port)))),
    )
}
