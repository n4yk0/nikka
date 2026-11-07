use super::theme::{CYAN, NEON, Rgb, Style};

const BLANK_GLYPH: [&str; 5] = [".....", ".....", ".....", ".....", "....."];

fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'N' => ["#...#", "##..#", "#.#.#", "#..##", "#...#"],
        '4' => ["#..#.", "#..#.", "#####", "...#.", "...#."],
        'Y' => ["#...#", ".#.#.", "..#..", "..#..", "..#.."],
        'K' => ["#...#", "#..#.", "###..", "#..#.", "#...#"],
        '0' => [".###.", "#..##", "#.#.#", "##..#", ".###."],
        '-' => [".....", ".....", ".###.", ".....", "....."],
        _ => BLANK_GLYPH,
    }
}

/// Renders text with the 5x5 pixel font, two columns per pixel, under a neon→cyan gradient.
pub fn big_text(text: &str) -> Vec<String> {
    let mut plain: [Vec<char>; 5] = Default::default();
    for c in text.to_uppercase().chars() {
        for (row, pattern) in plain.iter_mut().zip(glyph(c)) {
            for px in pattern.chars() {
                let cell = if px == '#' { '█' } else { ' ' };
                row.extend([cell, cell]);
            }
            row.extend([' ', ' ']);
        }
    }
    plain
        .iter()
        .map(|row| {
            let span = row.len().saturating_sub(1).max(1) as f64;
            let line: String = row
                .iter()
                .enumerate()
                .map(|(x, &c)| {
                    if c == ' ' {
                        " ".to_string()
                    } else {
                        Style::fg(blend_luv(NEON, CYAN, x as f64 / span)).paint("█")
                    }
                })
                .collect();
            line.trim_end_matches(' ').to_string()
        })
        .collect()
}

const WHITE_D65: [f64; 3] = [0.95047, 1.0, 1.08883];

/// CIE L*u*v* interpolation with a D65 white point: perceptually even, unlike a plain RGB blend.
pub fn blend_luv(from: Rgb, to: Rgb, t: f64) -> Rgb {
    let a = rgb_to_luv(from);
    let b = rgb_to_luv(to);
    luv_to_rgb([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2])])
}

fn rgb_to_luv(c: Rgb) -> [f64; 3] {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(c.0), lin(c.1), lin(c.2));
    let x = 0.412_390_799_265_959_5 * r + 0.357_584_339_383_878 * g + 0.180_480_788_401_834_3 * b;
    let y = 0.212_639_005_871_510_36 * r + 0.715_168_678_767_755_9 * g + 0.072_192_315_360_733_71 * b;
    let z = 0.019_330_818_715_591_85 * r + 0.119_194_779_794_625_99 * g + 0.950_532_152_249_660_6 * b;
    let yr = y / WHITE_D65[1];
    let l = if yr <= (6.0f64 / 29.0).powi(3) { yr * (29.0f64 / 3.0).powi(3) / 100.0 } else { 1.16 * yr.cbrt() - 0.16 };
    let (ubis, vbis) = uv(x, y, z);
    let (un, vn) = uv(WHITE_D65[0], WHITE_D65[1], WHITE_D65[2]);
    [l, 13.0 * l * (ubis - un), 13.0 * l * (vbis - vn)]
}

fn luv_to_rgb([l, u, v]: [f64; 3]) -> Rgb {
    let y = if l <= 0.08 {
        WHITE_D65[1] * l * 100.0 * (3.0f64 / 29.0).powi(3)
    } else {
        WHITE_D65[1] * ((l + 0.16) / 1.16).powi(3)
    };
    let (un, vn) = uv(WHITE_D65[0], WHITE_D65[1], WHITE_D65[2]);
    let (x, y, z) = if l == 0.0 {
        (0.0, 0.0, 0.0)
    } else {
        let ubis = u / (13.0 * l) + un;
        let vbis = v / (13.0 * l) + vn;
        (y * 9.0 * ubis / (4.0 * vbis), y, y * (12.0 - 3.0 * ubis - 20.0 * vbis) / (4.0 * vbis))
    };
    let r = 3.240_969_941_904_521_4 * x - 1.537_383_177_570_093_5 * y - 0.498_610_760_293_003_3 * z;
    let g = -0.969_243_636_280_879_8 * x + 1.875_967_501_507_720_7 * y + 0.041_555_057_407_175_61 * z;
    let b = 0.055_630_079_696_993_61 * x - 0.203_976_958_888_976_57 * y + 1.056_971_514_242_878_6 * z;
    let encode = |v: f64| {
        let v = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    Rgb(encode(r), encode(g), encode(b))
}

fn uv(x: f64, y: f64, z: f64) -> (f64, f64) {
    let denom = x + 15.0 * y + 3.0 * z;
    if denom == 0.0 { (0.0, 0.0) } else { (4.0 * x / denom, 9.0 * y / denom) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::ansi;

    #[test]
    fn big_text_has_five_rows_and_blank_unknown_glyphs() {
        let rows = big_text("N4YK0");
        assert_eq!(rows.len(), 5);
        assert!(ansi::width(&rows[2]) >= 5 * 10);
        assert!(ansi::strip(&big_text("?")[0]).trim().is_empty());
        assert_eq!(ansi::strip(&big_text("-")[2]), "  ██████");
    }

    #[test]
    fn luv_blend_hits_both_endpoints() {
        assert_eq!(blend_luv(NEON, CYAN, 0.0), NEON);
        assert_eq!(blend_luv(NEON, CYAN, 1.0), CYAN);
        let mid = blend_luv(NEON, CYAN, 0.5);
        assert!(mid.0 < 0x10 && mid.1 > CYAN.1 && mid.1 < NEON.1 && mid.2 > NEON.2 && mid.2 < CYAN.2, "{mid:?}");
    }

    #[test]
    fn luv_round_trips_greys() {
        for v in [0u8, 1, 128, 254, 255] {
            assert_eq!(blend_luv(Rgb(v, v, v), Rgb(v, v, v), 0.3), Rgb(v, v, v));
        }
    }
}
