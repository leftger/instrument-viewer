//! Temporary bisect harness: config read + fetch on one session, no worker.

#[allow(dead_code)]
#[path = "../src/scpi.rs"]
mod scpi;
#[allow(dead_code)]
#[path = "../src/transport.rs"]
mod transport;
#[allow(dead_code)]
#[path = "../src/usbtmc.rs"]
mod usbtmc;
#[allow(dead_code)]
#[path = "../src/waveform.rs"]
mod waveform;

// These refer to each other as `crate::<module>`, which resolves to the modules
// declared here because an example's crate root is this file.
#[allow(dead_code, unused_imports)]
#[path = "../src/afg.rs"]
mod afg;
#[allow(dead_code, unused_imports)]
#[path = "../src/backend.rs"]
mod backend;
#[allow(dead_code, unused_imports)]
#[path = "../src/config.rs"]
mod config;
#[allow(dead_code, unused_imports)]
#[path = "../src/daq4000a.rs"]
mod daq4000a;
#[allow(dead_code, unused_imports)]
#[path = "../src/dm3058.rs"]
mod dm3058;
#[allow(dead_code, unused_imports)]
#[path = "../src/ds1000z.rs"]
mod ds1000z;
#[allow(dead_code, unused_imports)]
#[path = "../src/dsa800.rs"]
mod dsa800;
#[allow(dead_code, unused_imports)]
#[path = "../src/hdm3000.rs"]
mod hdm3000;
#[allow(dead_code, unused_imports)]
#[path = "../src/hrdo2000.rs"]
mod hrdo2000;
#[allow(dead_code, unused_imports)]
#[path = "../src/keysight.rs"]
mod keysight;
#[allow(dead_code, unused_imports)]
#[path = "../src/profile.rs"]
mod profile;
#[allow(dead_code, unused_imports)]
#[path = "../src/registry.rs"]
mod registry;
#[allow(dead_code, unused_imports)]
#[path = "../src/rigol.rs"]
mod rigol;
#[allow(dead_code, unused_imports)]
#[path = "../src/sds.rs"]
mod sds;
#[allow(dead_code, unused_imports)]
#[path = "../src/siglent.rs"]
mod siglent;
#[allow(dead_code, unused_imports)]
#[path = "../src/siglent_ssa.rs"]
mod siglent_ssa;
#[allow(dead_code, unused_imports)]
#[path = "../src/tek.rs"]
mod tek;

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
    let idn = s.query("*IDN?")?;
    println!("IDN: {idn}");
    let backend = backend::from_idn(&idn);
    s.set_preamble(backend.preamble())?;

    for i in 0..cycles {
        let t = std::time::Instant::now();
        let cfg = config::read_config(&mut s, backend.as_ref())?;
        let tc = t.elapsed();
        let trace = backend.fetch_channel(&mut s, "CH1")?;
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
