use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::audio::AudioAlertController;

/// Signal or measurement source monitored by the alarm system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AlarmSource {
    #[default]
    MeterReading,
    SupplyVolts,
    SupplyAmps,
    ScopePkPk,
    ScopeMax,
    ScopeMin,
    ScopeFreq,
}

impl AlarmSource {
    pub const ALL: &[AlarmSource] = &[
        AlarmSource::MeterReading,
        AlarmSource::SupplyVolts,
        AlarmSource::SupplyAmps,
        AlarmSource::ScopePkPk,
        AlarmSource::ScopeMax,
        AlarmSource::ScopeMin,
        AlarmSource::ScopeFreq,
    ];

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::MeterReading => "DMM / DAQ Reading",
            Self::SupplyVolts => "Supply Voltage (V)",
            Self::SupplyAmps => "Supply Current (A)",
            Self::ScopePkPk => "Scope Pk-Pk (V)",
            Self::ScopeMax => "Scope Max (V)",
            Self::ScopeMin => "Scope Min (V)",
            Self::ScopeFreq => "Scope Freq (Hz)",
        }
    }
}

/// Configuration parameters for limit monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlarmConfig {
    pub enabled: bool,
    pub source: AlarmSource,
    pub high_enabled: bool,
    pub high_threshold: f64,
    pub low_enabled: bool,
    pub low_threshold: f64,
    pub hysteresis: f64,
    pub audio_enabled: bool,
}

impl Default for AlarmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source: AlarmSource::MeterReading,
            high_enabled: true,
            high_threshold: 5.0,
            low_enabled: false,
            low_threshold: 0.0,
            hysteresis: 0.05,
            audio_enabled: true,
        }
    }
}

/// Runtime state of the limit alarm system.
#[derive(Debug, Clone, Default)]
pub struct AlarmState {
    pub triggered: bool,
    pub triggered_high: bool,
    pub triggered_low: bool,
    pub message: String,
    pub last_sound_at: Option<Instant>,
}

pub struct AlarmManager {
    pub config: AlarmConfig,
    pub state: AlarmState,
    pub audio: AudioAlertController,
}

impl Default for AlarmManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AlarmManager {
    pub fn new() -> Self {
        Self {
            config: AlarmConfig::default(),
            state: AlarmState::default(),
            audio: AudioAlertController::new(),
        }
    }

    /// Evaluates a new measurement against active thresholds.
    pub fn evaluate(&mut self, value: f64) {
        if !self.config.enabled || !value.is_finite() {
            self.state.triggered = false;
            self.state.triggered_high = false;
            self.state.triggered_low = false;
            self.state.message.clear();
            return;
        }

        let mut high_viol = self.state.triggered_high;
        if self.config.high_enabled {
            if !high_viol && value >= self.config.high_threshold {
                high_viol = true;
            } else if high_viol
                && value < (self.config.high_threshold - self.config.hysteresis.max(0.0))
            {
                high_viol = false;
            }
        } else {
            high_viol = false;
        }

        let mut low_viol = self.state.triggered_low;
        if self.config.low_enabled {
            if !low_viol && value <= self.config.low_threshold {
                low_viol = true;
            } else if low_viol
                && value > (self.config.low_threshold + self.config.hysteresis.max(0.0))
            {
                low_viol = false;
            }
        } else {
            low_viol = false;
        }

        self.state.triggered_high = high_viol;
        self.state.triggered_low = low_viol;
        self.state.triggered = high_viol || low_viol;

        if high_viol {
            self.state.message = format!(
                "HIGH LIMIT: {:.4} >= {:.4} ({})",
                value,
                self.config.high_threshold,
                self.config.source.display_name()
            );
        } else if low_viol {
            self.state.message = format!(
                "LOW LIMIT: {:.4} <= {:.4} ({})",
                value,
                self.config.low_threshold,
                self.config.source.display_name()
            );
        } else {
            self.state.message.clear();
        }

        if self.state.triggered && self.config.audio_enabled {
            let now = Instant::now();
            let should_sound = self
                .state
                .last_sound_at
                .map(|t| now.duration_since(t) >= Duration::from_millis(600))
                .unwrap_or(true);

            if should_sound {
                self.state.last_sound_at = Some(now);
                self.audio.trigger_alarm();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alarm_high_threshold_and_hysteresis() {
        let mut mgr = AlarmManager::new();
        mgr.config.enabled = true;
        mgr.config.high_enabled = true;
        mgr.config.high_threshold = 5.0;
        mgr.config.hysteresis = 0.2;
        mgr.config.audio_enabled = false;

        mgr.evaluate(4.9);
        assert!(!mgr.state.triggered);

        mgr.evaluate(5.05);
        assert!(mgr.state.triggered);
        assert!(mgr.state.triggered_high);
        assert!(mgr.state.message.contains("HIGH LIMIT"));

        // Dropping within hysteresis band: remains triggered
        mgr.evaluate(4.9);
        assert!(mgr.state.triggered);

        // Dropping below hysteresis: clears
        mgr.evaluate(4.75);
        assert!(!mgr.state.triggered);
    }

    #[test]
    fn alarm_low_threshold_and_hysteresis() {
        let mut mgr = AlarmManager::new();
        mgr.config.enabled = true;
        mgr.config.high_enabled = false;
        mgr.config.low_enabled = true;
        mgr.config.low_threshold = 1.0;
        mgr.config.hysteresis = 0.1;
        mgr.config.audio_enabled = false;

        mgr.evaluate(1.5);
        assert!(!mgr.state.triggered);

        mgr.evaluate(0.95);
        assert!(mgr.state.triggered);
        assert!(mgr.state.triggered_low);
        assert!(mgr.state.message.contains("LOW LIMIT"));

        // Rising within hysteresis band
        mgr.evaluate(1.05);
        assert!(mgr.state.triggered);

        // Rising above hysteresis band
        mgr.evaluate(1.15);
        assert!(!mgr.state.triggered);
    }
}
