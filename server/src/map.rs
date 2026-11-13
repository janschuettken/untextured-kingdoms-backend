//! Einfacher deterministischer Kartengenerator mit diskreten Höhenstufen.

use protocol::MapInfo;

/// Erzeugt eine Karte aus ein paar "Hügeln", die auf Stufen gerundet werden.
pub fn generate(width: u32, height: u32, max_level: u8, seed: u64) -> MapInfo {
    let mut rng = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let mut next = || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((rng >> 33) as f32) / (u32::MAX >> 1) as f32
    };

    let hills: Vec<(f32, f32, f32, f32)> = (0..8)
        .map(|_| {
            let x = next() * width as f32;
            let y = next() * height as f32;
            let radius = 6.0 + next() * 10.0;
            let strength = 0.35 + next() * 0.6;
            (x, y, radius, strength)
        })
        .collect();

    let mut levels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let h: f32 = hills
                .iter()
                .map(|&(hx, hy, r, s)| {
                    let d2 = (x as f32 - hx).powi(2) + (y as f32 - hy).powi(2);
                    s * (-d2 / (r * r)).exp()
                })
                .sum();
            let level = (h * max_level as f32).floor().clamp(0.0, max_level as f32) as u8;
            levels.push(level);
        }
    }

    MapInfo { width, height, levels, max_level }
}
