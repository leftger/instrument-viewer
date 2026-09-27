use crate::waveform::ChannelTrace;

/// Detected peak in a waveform or spectrum trace.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakPoint {
    pub channel: String,
    pub x: f64,
    pub y: f64,
    pub x_unit: String,
    pub y_unit: String,
}

/// Peak detection result containing both maximum and minimum peaks.
#[derive(Debug, Clone, PartialEq)]
pub struct TracePeaks {
    pub max: Option<PeakPoint>,
    pub min: Option<PeakPoint>,
}

impl TracePeaks {
    /// Detects global maximum and minimum peaks in the given trace.
    pub fn find(trace: &ChannelTrace) -> Self {
        if trace.points.is_empty() {
            return Self {
                max: None,
                min: None,
            };
        }

        let mut max_p = trace.points[0];
        let mut min_p = trace.points[0];

        for &p in &trace.points[1..] {
            if p[1] > max_p[1] {
                max_p = p;
            }
            if p[1] < min_p[1] {
                min_p = p;
            }
        }

        Self {
            max: Some(PeakPoint {
                channel: trace.channel.clone(),
                x: max_p[0],
                y: max_p[1],
                x_unit: trace.x_unit.clone(),
                y_unit: trace.y_unit.clone(),
            }),
            min: Some(PeakPoint {
                channel: trace.channel.clone(),
                x: min_p[0],
                y: min_p[1],
                x_unit: trace.x_unit.clone(),
                y_unit: trace.y_unit.clone(),
            }),
        }
    }
}

/// User-pinned measurement marker.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedMarker {
    pub id: usize,
    pub channel: String,
    pub x: f64,
    pub y: f64,
    pub x_unit: String,
    pub y_unit: String,
}

/// Manager for up to 10 persistent pinned measurement markers.
#[derive(Debug, Clone, Default)]
pub struct MarkerManager {
    markers: Vec<PinnedMarker>,
    next_id: usize,
    pub peak_tracking_enabled: bool,
}

impl MarkerManager {
    pub const MAX_MARKERS: usize = 10;

    pub fn new() -> Self {
        Self {
            markers: Vec::with_capacity(Self::MAX_MARKERS),
            next_id: 1,
            peak_tracking_enabled: false,
        }
    }

    pub fn markers(&self) -> &[PinnedMarker] {
        &self.markers
    }

    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.markers.len()
    }

    /// Adds a pinned marker at `(x, y)`. Returns true if added.
    pub fn add_marker(
        &mut self,
        channel: impl Into<String>,
        x: f64,
        y: f64,
        x_unit: impl Into<String>,
        y_unit: impl Into<String>,
    ) -> bool {
        if self.markers.len() >= Self::MAX_MARKERS {
            return false;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.markers.push(PinnedMarker {
            id,
            channel: channel.into(),
            x,
            y,
            x_unit: x_unit.into(),
            y_unit: y_unit.into(),
        });
        true
    }

    /// Removes a marker by ID.
    #[allow(dead_code)]
    pub fn remove_marker(&mut self, id: usize) -> bool {
        if let Some(pos) = self.markers.iter().position(|m| m.id == id) {
            self.markers.remove(pos);
            true
        } else {
            false
        }
    }

    /// Removes the marker closest to `(x, y)` in normalized coordinate space.
    pub fn remove_nearest(&mut self, x: f64, y: f64) -> bool {
        if self.markers.is_empty() {
            return false;
        }
        let mut best_idx = 0;
        let mut best_dist = f64::MAX;

        for (i, m) in self.markers.iter().enumerate() {
            let dx = m.x - x;
            let dy = m.y - y;
            let dist = dx * dx + dy * dy;
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }

        self.markers.remove(best_idx);
        true
    }

    /// Clears all pinned markers.
    pub fn clear(&mut self) {
        self.markers.clear();
        self.next_id = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_peaks_finds_extrema() {
        let trace = ChannelTrace {
            channel: "CH1".to_string(),
            x_unit: "s".to_string(),
            y_unit: "V".to_string(),
            points: vec![
                [0.0, 1.0],
                [0.1, 5.5],
                [0.2, -2.3],
                [0.3, 3.0],
            ],
        };

        let peaks = TracePeaks::find(&trace);
        let max = peaks.max.unwrap();
        let min = peaks.min.unwrap();

        assert_eq!(max.x, 0.1);
        assert_eq!(max.y, 5.5);
        assert_eq!(min.x, 0.2);
        assert_eq!(min.y, -2.3);
    }

    #[test]
    fn marker_manager_operations() {
        let mut mgr = MarkerManager::new();
        assert!(mgr.is_empty());

        assert!(mgr.add_marker("CH1", 10.0, 2.5, "s", "V"));
        assert!(mgr.add_marker("CH1", 20.0, 3.5, "s", "V"));
        assert_eq!(mgr.len(), 2);
        assert_eq!(mgr.markers()[0].id, 1);
        assert_eq!(mgr.markers()[1].id, 2);

        assert!(mgr.remove_marker(1));
        assert_eq!(mgr.len(), 1);
        assert_eq!(mgr.markers()[0].id, 2);

        mgr.clear();
        assert!(mgr.is_empty());
    }
}
