/// False-color palettes for 3D spectrograms, waterfalls, and traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    #[default]
    Turbo,
    Viridis,
    Iron,
    Jet,
    Inferno,
}

impl Palette {
    pub const ALL: &[Palette] = &[
        Self::Turbo,
        Self::Viridis,
        Self::Iron,
        Self::Jet,
        Self::Inferno,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Turbo => "Turbo",
            Self::Viridis => "Viridis",
            Self::Iron => "Iron",
            Self::Jet => "Jet",
            Self::Inferno => "Inferno",
        }
    }

    pub fn next(self) -> Self {
        let all = Self::ALL;
        let idx = all.iter().position(|p| *p == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }

    /// Maps normalized scalar `0.0..=1.0` to RGB bytes `[r, g, b]`.
    pub fn map_normalized(self, t: f32) -> [u8; 3] {
        let t = t.clamp(0.0, 1.0);
        let idx = (t * 255.0).round() as usize;
        match self {
            Self::Turbo => TURBO_PALETTE[idx.min(255)],
            Self::Viridis => VIRIDIS_PALETTE[idx.min(255)],
            Self::Iron => IRON_PALETTE[idx.min(255)],
            Self::Jet => JET_PALETTE[idx.min(255)],
            Self::Inferno => INFERNO_PALETTE[idx.min(255)],
        }
    }
}

// Generate smooth Viridis, Turbo, Jet, Iron, and Inferno palettes
fn sample_gradient(stops: &[(f32, [u8; 3])]) -> [[u8; 3]; 256] {
    let mut table = [[0u8; 3]; 256];
    for i in 0..256 {
        let t = i as f32 / 255.0;
        let mut idx = 0;
        while idx < stops.len() - 1 && stops[idx + 1].0 <= t {
            idx += 1;
        }
        if idx >= stops.len() - 1 {
            table[i] = stops[stops.len() - 1].1;
        } else {
            let (t0, c0) = stops[idx];
            let (t1, c1) = stops[idx + 1];
            let factor = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            let r = (c0[0] as f32 + (c1[0] as f32 - c0[0] as f32) * factor).round() as u8;
            let g = (c0[1] as f32 + (c1[1] as f32 - c0[1] as f32) * factor).round() as u8;
            let b = (c0[2] as f32 + (c1[2] as f32 - c0[2] as f32) * factor).round() as u8;
            table[i] = [r, g, b];
        }
    }
    table
}

use std::sync::LazyLock;

static TURBO_PALETTE: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    sample_gradient(&[
        (0.00, [48, 18, 59]),
        (0.15, [70, 134, 251]),
        (0.35, [27, 229, 181]),
        (0.55, [164, 252, 60]),
        (0.75, [251, 185, 56]),
        (0.90, [227, 68, 25]),
        (1.00, [122, 4, 3]),
    ])
});

static VIRIDIS_PALETTE: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    sample_gradient(&[
        (0.00, [68, 1, 84]),
        (0.25, [59, 82, 139]),
        (0.50, [33, 145, 140]),
        (0.75, [94, 201, 98]),
        (1.00, [253, 231, 37]),
    ])
});

static IRON_PALETTE: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    sample_gradient(&[
        (0.00, [0, 0, 10]),
        (0.25, [80, 0, 130]),
        (0.50, [200, 30, 80]),
        (0.75, [255, 160, 20]),
        (1.00, [255, 255, 220]),
    ])
});

static JET_PALETTE: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    sample_gradient(&[
        (0.00, [0, 0, 143]),
        (0.15, [0, 0, 255]),
        (0.40, [0, 255, 255]),
        (0.65, [255, 255, 0]),
        (0.90, [255, 0, 0]),
        (1.00, [128, 0, 0]),
    ])
});

static INFERNO_PALETTE: LazyLock<[[u8; 3]; 256]> = LazyLock::new(|| {
    sample_gradient(&[
        (0.00, [0, 0, 4]),
        (0.25, [87, 16, 110]),
        (0.50, [187, 55, 84]),
        (0.75, [249, 142, 9]),
        (1.00, [252, 255, 164]),
    ])
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_mapping_bounds() {
        for pal in Palette::ALL {
            let c0 = pal.map_normalized(0.0);
            let c1 = pal.map_normalized(1.0);
            assert_ne!(c0, c1);
            let mid = pal.map_normalized(0.5);
            assert!(mid[0] > 0 || mid[1] > 0 || mid[2] > 0);
        }
    }

    #[test]
    fn palette_cycling() {
        let p = Palette::Turbo;
        assert_eq!(p.next(), Palette::Viridis);
    }
}
