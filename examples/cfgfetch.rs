//! Temporary bisect harness: config read + fetch on one session, no worker.

#[allow(dead_code)]
#[path = "../src/scpi.rs"]
mod scpi;
#[allow(dead_code)]
#[path = "../src/waveform.rs"]
mod waveform;

// config.rs refers to `crate::scpi`, which resolves to the module above
// because an example's crate root is this file.
#[allow(dead_code, unused_imports)]
#[path = "../src/config.rs"]
mod config;

use scpi::ScpiSession;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "169.254.6.252:4000".into());
    let cycles: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let mut s = ScpiSession::connect(&addr, std::time::Duration::from_secs(6))?;
    println!("IDN: {}", s.query("*IDN?")?);

    for i in 0..cycles {
        let t = std::time::Instant::now();
        let cfg = config::read_config(&mut s)?;
        let tc = t.elapsed();
        let trace = waveform::fetch_channel(&mut s, "CH1")?;
        println!(
            "cycle {i}: cfg {:?} fetch {:?} pts={} rl={}",
            tc,
            t.elapsed() - tc,
            trace.points.len(),
            cfg.horizontal.record_length
        );
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(())
}
