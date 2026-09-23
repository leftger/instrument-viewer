//! Stacked view: one band per channel, each scaled to its own extremes.
//!
//! On a shared vertical axis anything small disappears — a 50 mV ripple next
//! to a 5 V square wave is a flat line. Stacking gives every channel the same
//! height and its own gain, at the cost of a y axis that no longer reads in
//! volts, so `Lane::value` maps a plotted y back for readouts.

use crate::waveform::ChannelTrace;

/// Share of a lane's one-unit height the trace may use, so channels at full
/// deflection keep a visible gap between them.
const FILL: f64 = 0.9;

/// Affine map between one channel's values and its band on the shared axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lane {
    /// Plot y the channel's mid value sits at. Lanes are one unit apart.
    pub center: f64,
    mid: f64,
    gain: f64,
}

impl Lane {
    /// `index` counts from the top, so CH1 stays above CH2.
    pub fn new(index: usize, count: usize, min: f64, max: f64) -> Self {
        let span = max - min;
        Self {
            center: count.saturating_sub(1).saturating_sub(index) as f64,
            mid: (min + max) / 2.0,
            // A flat trace has no scale of its own; draw it along its middle.
            gain: if span.is_finite() && span > 0.0 {
                FILL / span
            } else {
                0.0
            },
        }
    }

    pub fn plot_y(&self, value: f64) -> f64 {
        self.center + (value - self.mid) * self.gain
    }

    pub fn value(&self, plot_y: f64) -> f64 {
        if self.gain > 0.0 {
            self.mid + (plot_y - self.center) / self.gain
        } else {
            self.mid
        }
    }
}

/// One lane per trace, top to bottom in the order given.
pub fn lanes(traces: &[ChannelTrace]) -> Vec<Lane> {
    traces
        .iter()
        .enumerate()
        .map(|(i, trace)| {
            let (min, max) = trace
                .points
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(p[1]), hi.max(p[1]))
                });
            if min.is_finite() && max.is_finite() {
                Lane::new(i, traces.len(), min, max)
            } else {
                Lane::new(i, traces.len(), 0.0, 0.0)
            }
        })
        .collect()
}

/// Lane a plot y belongs to, for turning a hovered point back into volts.
pub fn nearest(lanes: &[Lane], plot_y: f64) -> Option<usize> {
    lanes
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (a.center - plot_y)
                .abs()
                .total_cmp(&(b.center - plot_y).abs())
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(channel: &str, values: &[f64]) -> ChannelTrace {
        ChannelTrace {
            channel: channel.into(),
            x_unit: "s".into(),
            y_unit: "V".into(),
            points: values
                .iter()
                .enumerate()
                .map(|(i, v)| [i as f64, *v])
                .collect(),
        }
    }

    #[test]
    fn each_channel_fills_its_own_lane() {
        let lanes = lanes(&[trace("CH1", &[-5.0, 5.0]), trace("CH2", &[0.0, 0.05])]);
        // Very different amplitudes, identical drawn height.
        let big = lanes[0].plot_y(5.0) - lanes[0].plot_y(-5.0);
        let small = lanes[1].plot_y(0.05) - lanes[1].plot_y(0.0);
        assert!((big - small).abs() < 1e-12);
        assert!(big <= 1.0, "lanes would overlap: {big}");
    }

    #[test]
    fn first_channel_sits_on_top() {
        let lanes = lanes(&[
            trace("CH1", &[-1.0, 1.0]),
            trace("CH2", &[-1.0, 1.0]),
            trace("CH3", &[-1.0, 1.0]),
        ]);
        assert_eq!(lanes[0].center, 2.0);
        assert_eq!(lanes[2].center, 0.0);
        assert!(lanes[0].plot_y(-1.0) > lanes[1].plot_y(1.0), "lanes touch");
    }

    #[test]
    fn plotted_y_maps_back_to_volts() {
        let lane = Lane::new(1, 3, -2.0, 6.0);
        for v in [-2.0, 0.0, 1.5, 6.0] {
            assert!((lane.value(lane.plot_y(v)) - v).abs() < 1e-12);
        }
    }

    #[test]
    fn offset_does_not_shift_a_trace_out_of_its_lane() {
        // 100 V of DC on a 1 V signal still draws inside the band.
        let lane = Lane::new(0, 2, 99.5, 100.5);
        assert!((lane.plot_y(100.5) - lane.center).abs() <= 0.5);
        assert!((lane.plot_y(99.5) - lane.center).abs() <= 0.5);
    }

    #[test]
    fn flat_trace_draws_along_its_lane_centre() {
        let lanes = lanes(&[trace("CH1", &[3.0, 3.0, 3.0])]);
        assert_eq!(lanes[0].plot_y(3.0), lanes[0].center);
        assert_eq!(lanes[0].value(lanes[0].center), 3.0);
    }

    #[test]
    fn empty_trace_is_finite() {
        let lanes = lanes(&[trace("CH1", &[])]);
        assert!(lanes[0].plot_y(0.0).is_finite());
    }

    #[test]
    fn nearest_picks_the_hovered_band() {
        let lanes = lanes(&[trace("CH1", &[-1.0, 1.0]), trace("CH2", &[-1.0, 1.0])]);
        assert_eq!(nearest(&lanes, 0.9), Some(0));
        assert_eq!(nearest(&lanes, 0.1), Some(1));
        assert_eq!(nearest(&[], 0.0), None);
    }
}
