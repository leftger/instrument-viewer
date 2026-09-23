use crate::waveform::ChannelTrace;

#[derive(Clone, Debug, PartialEq)]
pub struct Measurements {
    pub min: f64,
    pub max: f64,
    pub pk_pk: f64,
    pub mean: f64,
    pub rms: f64,
    pub period_s: Option<f64>,
    pub frequency_hz: Option<f64>,
}

pub fn measure(trace: &ChannelTrace) -> Option<Measurements> {
    if trace.points.is_empty() {
        return None;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for p in &trace.points {
        let v = p[1];
        min = min.min(v);
        max = max.max(v);
        sum += v;
        sum_sq += v * v;
    }
    let n = trace.points.len() as f64;
    let mean = sum / n;
    let rms = (sum_sq / n).sqrt();
    let pk_pk = max - min;
    let (period_s, frequency_hz) = period_from_crossings(&trace.points, mean, pk_pk);
    Some(Measurements {
        min,
        max,
        pk_pk,
        mean,
        rms,
        period_s,
        frequency_hz,
    })
}

/// Rising crossings of `mean`, with hysteresis of 5% of pk-pk (min 1e-12 V).
fn period_from_crossings(points: &[[f64; 2]], mean: f64, pk_pk: f64) -> (Option<f64>, Option<f64>) {
    if points.len() < 3 {
        return (None, None);
    }
    let hyst = (pk_pk * 0.05).max(1e-12);
    let mut times = Vec::new();
    let mut armed = false;
    for w in points.windows(2) {
        let (t0, v0) = (w[0][0], w[0][1]);
        let (t1, v1) = (w[1][0], w[1][1]);
        if v0 < mean - hyst {
            armed = true;
        }
        if armed && v0 < mean && v1 >= mean {
            let dv = v1 - v0;
            let t = if dv.abs() < f64::EPSILON {
                t1
            } else {
                t0 + (mean - v0) / dv * (t1 - t0)
            };
            times.push(t);
            armed = false;
        }
    }
    if times.len() < 2 {
        return (None, None);
    }
    let mut acc = 0.0;
    for pair in times.windows(2) {
        acc += pair[1] - pair[0];
    }
    let period = acc / (times.len() - 1) as f64;
    if period <= 0.0 || !period.is_finite() {
        return (None, None);
    }
    (Some(period), Some(1.0 / period))
}

pub fn value_at(trace: &ChannelTrace, t: f64) -> Option<f64> {
    let pts = &trace.points;
    if pts.is_empty() {
        return None;
    }
    if t <= pts[0][0] {
        return Some(pts[0][1]);
    }
    if t >= pts[pts.len() - 1][0] {
        return Some(pts[pts.len() - 1][1]);
    }
    let mut lo = 0usize;
    let mut hi = pts.len() - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if pts[mid][0] <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, v0) = (pts[lo][0], pts[lo][1]);
    let (t1, v1) = (pts[hi][0], pts[hi][1]);
    let dt = t1 - t0;
    if dt.abs() < f64::EPSILON {
        Some(v0)
    } else {
        Some(v0 + (t - t0) / dt * (v1 - v0))
    }
}

pub fn format_si(value: f64, unit: &str) -> String {
    let abs = value.abs();
    let (scaled, prefix) = if abs >= 1e6 {
        (value / 1e6, "M")
    } else if abs >= 1e3 {
        (value / 1e3, "k")
    } else if abs >= 1.0 {
        (value, "")
    } else if abs >= 1e-3 {
        (value * 1e3, "m")
    } else if abs >= 1e-6 {
        (value * 1e6, "µ")
    } else if abs >= 1e-9 {
        (value * 1e9, "n")
    } else {
        (value * 1e12, "p")
    };
    format!("{scaled:.4} {prefix}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waveform::ChannelTrace;

    fn sine(freq: f64, n: usize, dt: f64) -> ChannelTrace {
        let points = (0..n)
            .map(|i| {
                let t = i as f64 * dt;
                [t, (2.0 * std::f64::consts::PI * freq * t).sin()]
            })
            .collect();
        ChannelTrace {
            channel: "CH1".into(),
            x_unit: "s".into(),
            y_unit: "V".into(),
            points,
        }
    }

    #[test]
    fn sine_stats_and_frequency() {
        let t = sine(1_000.0, 10_000, 1e-6);
        let m = measure(&t).unwrap();
        assert!((m.max - 1.0).abs() < 0.01);
        assert!((m.min + 1.0).abs() < 0.01);
        assert!((m.pk_pk - 2.0).abs() < 0.02);
        assert!(m.mean.abs() < 0.02);
        assert!((m.rms - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.02);
        let f = m.frequency_hz.unwrap();
        assert!((f - 1000.0).abs() < 2.0, "freq={f}");
        let p = m.period_s.unwrap();
        assert!((p - 0.001).abs() < 2e-6);
    }

    #[test]
    fn interpolates_between_samples() {
        let t = ChannelTrace {
            channel: "CH1".into(),
            x_unit: "s".into(),
            y_unit: "V".into(),
            points: vec![[0.0, 0.0], [2.0, 10.0]],
        };
        assert!((value_at(&t, 1.0).unwrap() - 5.0).abs() < 1e-12);
    }
}
