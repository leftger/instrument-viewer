//! Headless connectivity check: `cargo run --example probe -- 169.254.6.252:4000 CH1`
use std::time::{Duration, Instant};

#[allow(dead_code)]
#[path = "../src/scpi.rs"]
mod scpi;
#[allow(dead_code)]
#[path = "../src/waveform.rs"]
mod waveform;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "169.254.6.252:4000".into());
    let ch = std::env::args().nth(2).unwrap_or_else(|| "CH1".into());

    let t0 = Instant::now();
    let mut s = scpi::ScpiSession::connect(&addr, Duration::from_secs(4))?;
    println!("connected in {:?}", t0.elapsed());
    println!("IDN: {}", s.query("*IDN?")?);

    let rounds: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    for round in 1..=rounds {
        let t = Instant::now();
        let trace = waveform::fetch_channel(&mut s, &ch)?;
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for p in &trace.points {
            lo = lo.min(p[1]);
            hi = hi.max(p[1]);
        }
        println!(
            "round {round}: {} pts in {:?}  Vmin={lo:.4} Vmax={hi:.4}  units={}/{}",
            trace.points.len(),
            t.elapsed(),
            trace.x_unit,
            trace.y_unit
        );
    }
    Ok(())
}
