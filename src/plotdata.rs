//! Screen-resolution reduction for plotted traces.
//!
//! `egui_plot` transforms and tessellates every point it is handed, every
//! frame, with no culling. A 10k-point record per channel is enough to stall a
//! maximized window, so reduce to roughly the pixel columns actually available.

/// Min/max decimation: for each bucket keep the extremes, in time order.
///
/// Peaks survive, unlike plain subsampling, so a narrow glitch still shows.
/// The result always spans the whole record — never just the visible slice —
/// because the plot derives its auto-bounds from the points it is given.
pub fn decimate(points: &[[f64; 2]], target: usize) -> Vec<[f64; 2]> {
    let buckets = target / 2;
    if buckets < 2 || points.len() <= target {
        return points.to_vec();
    }

    let mut out = Vec::with_capacity(buckets * 2 + 2);
    for b in 0..buckets {
        let start = b * points.len() / buckets;
        let end = ((b + 1) * points.len() / buckets).max(start + 1);
        let Some(slice) = points.get(start..end.min(points.len())) else {
            continue;
        };
        let mut lo = slice[0];
        let mut hi = slice[0];
        let mut lo_i = 0usize;
        let mut hi_i = 0usize;
        for (i, p) in slice.iter().enumerate() {
            if p[1] < lo[1] {
                lo = *p;
                lo_i = i;
            }
            if p[1] > hi[1] {
                hi = *p;
                hi_i = i;
            }
        }
        if lo_i <= hi_i {
            out.push(lo);
            if hi_i != lo_i {
                out.push(hi);
            }
        } else {
            out.push(hi);
            out.push(lo);
        }
    }

    // Keep the true endpoints so the time axis still covers the full record.
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        if out.first().map(|p| p[0]) != Some(first[0]) {
            out.insert(0, *first);
        }
        if out.last().map(|p| p[0]) != Some(last[0]) {
            out.push(*last);
        }
    }
    out
}

/// Reduce a trace to what a plot `width_px` wide can actually show.
///
/// With `clip` the series is first cut to the visible x-range, which bounds the
/// drawn point count at roughly two per column no matter how far in the user
/// zooms. Clipping must stay off while the plot is auto-fitting, because then
/// the plot takes its bounds from these points and would shrink onto itself.
pub fn prepare(
    points: &[[f64; 2]],
    x_min: f64,
    x_max: f64,
    width_px: f64,
    clip: bool,
) -> Vec<[f64; 2]> {
    let target = ((width_px.max(64.0) * 2.0) as usize).max(256);
    if !clip {
        return decimate(points, target);
    }
    // One sample of margin each side so the line still enters from off-screen.
    let lo = points.partition_point(|p| p[0] < x_min).saturating_sub(1);
    let hi = (points.partition_point(|p| p[0] <= x_max) + 1).min(points.len());
    match points.get(lo..hi) {
        Some(slice) => decimate(slice, target),
        None => decimate(points, target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<[f64; 2]> {
        (0..n).map(|i| [i as f64, i as f64]).collect()
    }

    #[test]
    fn short_series_is_untouched() {
        let p = ramp(100);
        assert_eq!(decimate(&p, 400), p);
    }

    #[test]
    fn reduces_and_keeps_endpoints() {
        let p = ramp(10_000);
        let out = decimate(&p, 800);
        assert!(out.len() <= 900, "len={}", out.len());
        assert!(out.len() >= 400);
        assert_eq!(out.first().unwrap()[0], 0.0);
        assert_eq!(out.last().unwrap()[0], 9_999.0);
    }

    #[test]
    fn preserves_a_single_sample_spike() {
        let mut p = ramp(10_000);
        for q in p.iter_mut() {
            q[1] = 0.0;
        }
        p[5_000][1] = 42.0;
        let out = decimate(&p, 800);
        assert!(out.iter().any(|q| q[1] == 42.0), "spike was dropped");
    }

    #[test]
    fn time_order_is_monotonic() {
        let p: Vec<[f64; 2]> = (0..5_000)
            .map(|i| [i as f64, ((i as f64) * 0.05).sin()])
            .collect();
        let out = decimate(&p, 600);
        assert!(out.windows(2).all(|w| w[1][0] >= w[0][0]));
    }

    #[test]
    fn clipping_bounds_the_drawn_count_when_zoomed_in() {
        let p = ramp(10_000);
        let all = prepare(&p, 0.0, 9_999.0, 1000.0, false);
        let zoomed = prepare(&p, 4_000.0, 4_100.0, 1000.0, true);
        assert!(all.len() <= 2_100, "len={}", all.len());
        assert!(zoomed.len() <= 2_100, "len={}", zoomed.len());
        // Only the visible window, not the whole record.
        assert!(zoomed.first().unwrap()[0] >= 3_999.0);
        assert!(zoomed.last().unwrap()[0] <= 4_101.0);
    }

    #[test]
    fn unclipped_prepare_spans_the_record() {
        let p = ramp(10_000);
        let out = prepare(&p, 4_000.0, 4_100.0, 1000.0, false);
        assert_eq!(out.first().unwrap()[0], 0.0);
        assert_eq!(out.last().unwrap()[0], 9_999.0);
    }

    #[test]
    fn panning_off_the_data_draws_nothing_expensive() {
        let p = ramp(10_000);
        let out = prepare(&p, 50_000.0, 60_000.0, 1000.0, true);
        assert!(out.len() <= 2, "len={}", out.len());
    }
}
