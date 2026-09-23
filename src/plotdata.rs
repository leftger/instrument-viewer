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

/// How many points are worth drawing for a plot `width_px` wide.
///
/// Scales with zoom so a magnified region keeps roughly two points per column
/// even though the reduced series still covers the entire record.
pub fn target_points(width_px: f64, full_span: f64, visible_span: f64, raw_len: usize) -> usize {
    let columns = width_px.max(64.0);
    let ratio = if visible_span > 0.0 && full_span > 0.0 {
        (full_span / visible_span).clamp(1.0, 512.0)
    } else {
        1.0
    };
    ((columns * 2.0 * ratio) as usize).clamp(256, raw_len.max(256))
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
    fn target_scales_with_zoom() {
        let wide = target_points(1000.0, 1.0, 1.0, 10_000);
        let zoomed = target_points(1000.0, 1.0, 0.1, 10_000);
        assert!(zoomed > wide);
        assert!(zoomed <= 10_000);
    }
}
