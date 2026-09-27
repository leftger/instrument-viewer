use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Controller for synthesized audio alerts.
pub struct AudioAlertController {
    sender: Sender<AudioCommand>,
    #[allow(dead_code)]
    enabled: Arc<AtomicBool>,
}

enum AudioCommand {
    TriggerAlarm,
}

impl AudioAlertController {
    pub fn new() -> Self {
        let (sender, receiver) = channel::<AudioCommand>();
        let enabled = Arc::new(AtomicBool::new(true));
        let enabled_thread = enabled.clone();

        thread::spawn(move || {
            // Attempt to initialize rodio output device sink safely
            let mut device_sink = rodio::stream::DeviceSinkBuilder::from_default_device()
                .ok()
                .and_then(|b| b.open_stream().ok());

            if let Some(sink) = &mut device_sink {
                sink.log_on_drop(false);
            }

            while let Ok(cmd) = receiver.recv() {
                match cmd {
                    AudioCommand::TriggerAlarm => {
                        if !enabled_thread.load(Ordering::Relaxed) {
                            continue;
                        }

                        if let Some(sink) = &device_sink {
                            let sample_rate = 22050u32;
                            let make_beep = |freq: f32, duration_secs: f32| -> Vec<f32> {
                                let num_samples = (sample_rate as f32 * duration_secs) as usize;
                                (0..num_samples)
                                    .map(|i| {
                                        let t = i as f32 / sample_rate as f32;
                                        let env = (t / 0.01).min(1.0)
                                            * ((duration_secs - t) / 0.01).min(1.0);
                                        (2.0 * std::f32::consts::PI * freq * t).sin() * env * 0.25
                                    })
                                    .collect()
                            };

                            let mut samples = make_beep(880.0, 0.12);
                            let gap = (sample_rate as f32 * 0.06) as usize;
                            samples.extend(std::iter::repeat_n(0.0, gap));
                            samples.extend(make_beep(1174.66, 0.15));

                            if let (Some(channels), Some(rate)) = (
                                std::num::NonZeroU16::new(1),
                                std::num::NonZeroU32::new(sample_rate),
                            ) {
                                let source =
                                    rodio::buffer::SamplesBuffer::new(channels, rate, samples);
                                sink.mixer().add(source);
                            }
                        }

                        // Cooldown pause to prevent spamming audio threads
                        thread::sleep(Duration::from_millis(400));
                    }
                }
            }
        });

        Self { sender, enabled }
    }

    /// Triggers an audible alarm notification.
    pub fn trigger_alarm(&self) {
        let _ = self.sender.send(AudioCommand::TriggerAlarm);
    }

    /// Sets whether audio alerts are enabled.
    #[allow(dead_code)]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Returns whether audio alerts are enabled.
    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

impl Default for AudioAlertController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_controller_toggle_and_trigger() {
        let ctrl = AudioAlertController::new();
        assert!(ctrl.is_enabled());
        ctrl.set_enabled(false);
        assert!(!ctrl.is_enabled());
        ctrl.trigger_alarm();
        ctrl.set_enabled(true);
        assert!(ctrl.is_enabled());
        ctrl.trigger_alarm();
    }
}
