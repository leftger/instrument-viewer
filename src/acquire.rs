use std::thread;
use std::time::{Duration, Instant};

use crate::scpi::{ScpiError, ScpiSession};

/// Arm a single-sequence acquisition and wait until the scope stops.
///
/// Restores the previous `STOPAFTER` setting and leaves the scope stopped so
/// Auto/Fetch do not keep the instrument busy.
pub fn wait_sequence(session: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError> {
    let previous = session.query("ACQUIRE:STOPAFTER?")?;
    session.write("ACQUIRE:STOPAFTER SEQUENCE")?;
    session.write("ACQUIRE:STATE ON")?;

    let start = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(200));
        let state = session.query("ACQUIRE:STATE?")?;
        let running = matches!(state.trim(), "1" | "ON" | "RUN");
        if !running {
            break;
        }
        if start.elapsed() > timeout {
            let _ = session.write(&format!("ACQUIRE:STOPAFTER {previous}"));
            return Err(ScpiError::Timeout);
        }
    }

    let _ = session.write(&format!("ACQUIRE:STOPAFTER {previous}"));
    Ok(())
}
