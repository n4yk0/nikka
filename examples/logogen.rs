//! Converts the source PNG into the pixel grid read by `src/logo` (`cargo run --example logogen`).

use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

const CELL_TRANSPARENT: char = '.';
const CELL_OUTLINE: char = 'K';
const CELL_WHITE: char = 'W';
const CELL_SHADE: char = 'L';
const CELL_BLUE: char = 'B';

#[derive(Parser)]
struct Args {
    /// Source PNG
    #[arg(long = "in", default_value = "assets/logo.png")]
    input: PathBuf,
    /// Destination grid file
    #[arg(long, default_value = "src/logo/grid.txt")]
    out: PathBuf,
    /// Grid width in pixels
    #[arg(long, default_value_t = 30)]
    width: usize,
}

/// RGBA pixels read premultiplied by alpha, which darkens anti-aliased edges; the committed grid depends on it.
struct Image {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
}

impl Image {
    fn pixel(&self, x: usize, y: usize) -> (f64, f64, f64, u8) {
        let i = (y * self.width + x) * 4;
        let a = self.rgba[i + 3];
        let premultiply = |c: u8| f64::from((u32::from(c) * 257 * u32::from(a) / 255) >> 8);
        (premultiply(self.rgba[i]), premultiply(self.rgba[i + 1]), premultiply(self.rgba[i + 2]), a)
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let img = match decode(&args.input) {
        Ok(img) => img,
        Err(e) => {
            eprintln!("{}: {e}", args.input.display());
            return ExitCode::FAILURE;
        }
    };
    let grid = pixelate(&img, args.width);
    if let Err(e) = fs::write(&args.out, grid_source(&grid)) {
        eprintln!("{}: {e}", args.out.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {} ({}x{})", args.out.display(), args.width, grid.len());
    ExitCode::SUCCESS
}

fn decode(path: &Path) -> Result<Image, String> {
    let mut decoder = png::Decoder::new(BufReader::new(File::open(path).map_err(|e| e.to_string())?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let pixels = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed colour was not expanded".to_string()),
    };
    Ok(Image { width: info.width as usize, height: info.height as usize, rgba })
}

fn opaque_bounds(img: &Image) -> Rect {
    let mut r = Rect { min_x: img.width, min_y: img.height, max_x: 0, max_y: 0 };
    for y in 0..img.height {
        for x in 0..img.width {
            if img.pixel(x, y).3 >= 128 {
                r = Rect {
                    min_x: r.min_x.min(x),
                    min_y: r.min_y.min(y),
                    max_x: r.max_x.max(x + 1),
                    max_y: r.max_y.max(y + 1),
                };
            }
        }
    }
    r
}

fn pixelate(img: &Image, width: usize) -> Vec<String> {
    let bx = opaque_bounds(img);
    let scale = (bx.max_x - bx.min_x) as f64 / width as f64;
    let mut height = ((bx.max_y - bx.min_y) as f64 / scale).round() as usize;
    height += height % 2;
    let side = scale.ceil() as usize;
    (0..height)
        .map(|gy| {
            (0..width)
                .map(|gx| {
                    let x0 = bx.min_x + (gx as f64 * scale) as usize;
                    let y0 = bx.min_y + (gy as f64 * scale) as usize;
                    classify(img, Rect { min_x: x0, min_y: y0, max_x: x0 + side, max_y: y0 + side })
                })
                .collect()
        })
        .collect()
}

/// Blocks overhanging the image count their outside pixels as transparent.
fn classify(img: &Image, block: Rect) -> char {
    let (mut total, mut transparent, mut dark) = (0usize, 0usize, 0usize);
    let (mut sum_r, mut sum_g, mut sum_b) = (0.0, 0.0, 0.0);
    for y in block.min_y..block.max_y {
        for x in block.min_x..block.max_x {
            total += 1;
            if x >= img.width || y >= img.height {
                transparent += 1;
                continue;
            }
            let (r, g, b, a) = img.pixel(x, y);
            if a < 128 {
                transparent += 1;
                continue;
            }
            if luminance(r, g, b) < 60.0 && b - r < 40.0 {
                dark += 1;
            }
            sum_r += r;
            sum_g += g;
            sum_b += b;
        }
    }
    let opaque = total - transparent;
    if total == 0 || transparent as f64 / total as f64 > 0.5 {
        return CELL_TRANSPARENT;
    }
    if dark as f64 / opaque as f64 >= 0.35 {
        return CELL_OUTLINE;
    }
    let (r, g, b) = (sum_r / opaque as f64, sum_g / opaque as f64, sum_b / opaque as f64);
    if b - r > 40.0 && luminance(r, g, b) < 170.0 {
        CELL_BLUE
    } else if b - r > 12.0 {
        CELL_SHADE
    } else {
        CELL_WHITE
    }
}

fn luminance(r: f64, g: f64, b: f64) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn grid_source(grid: &[String]) -> String {
    grid.iter().map(|line| format!("{line}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> Image {
        let mut img = Image { width: 100, height: 100, rgba: vec![0; 100 * 100 * 4] };
        let mut paint = |r: Rect, c: [u8; 4]| {
            for y in r.min_y..r.max_y {
                for x in r.min_x..r.max_x {
                    let i = (y * 100 + x) * 4;
                    img.rgba[i..i + 4].copy_from_slice(&c);
                }
            }
        };
        paint(Rect { min_x: 20, min_y: 20, max_x: 80, max_y: 80 }, [0, 0, 0, 255]);
        paint(Rect { min_x: 30, min_y: 30, max_x: 70, max_y: 50 }, [255, 255, 255, 255]);
        paint(Rect { min_x: 30, min_y: 50, max_x: 70, max_y: 60 }, [0, 32, 96, 255]);
        paint(Rect { min_x: 30, min_y: 60, max_x: 70, max_y: 70 }, [232, 240, 255, 255]);
        img
    }

    #[test]
    fn opaque_bounds_crop_transparency() {
        assert_eq!(opaque_bounds(&synthetic()), Rect { min_x: 20, min_y: 20, max_x: 80, max_y: 80 });
    }

    #[test]
    fn pixelate_classifies_the_palette() {
        assert_eq!(pixelate(&synthetic(), 6), ["KKKKKK", "KWWWWK", "KWWWWK", "KBBBBK", "KLLLLK", "KKKKKK"]);
    }

    #[test]
    fn pixelate_keeps_an_even_height() {
        let img = Image { width: 10, height: 15, rgba: vec![255; 10 * 15 * 4] };
        assert_eq!(pixelate(&img, 10).len() % 2, 0);
    }

    #[test]
    fn transparent_block() {
        let img = Image { width: 4, height: 4, rgba: vec![0; 64] };
        assert_eq!(classify(&img, Rect { min_x: 0, min_y: 0, max_x: 4, max_y: 4 }), CELL_TRANSPARENT);
    }

    #[test]
    fn grid_source_is_one_row_per_line() {
        assert_eq!(grid_source(&["KW".to_string(), "LB".to_string()]), "KW\nLB\n");
    }

    #[test]
    fn decode_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(decode(&dir.path().join("missing.png")).is_err());
        let bad = dir.path().join("bad.png");
        fs::write(&bad, "not a png").unwrap();
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn committed_grid_matches_the_source_png() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let img = decode(&root.join("assets/logo.png")).unwrap();
        let committed = fs::read_to_string(root.join("src/logo/grid.txt")).unwrap();
        assert_eq!(grid_source(&pixelate(&img, 30)), committed);
    }
}
