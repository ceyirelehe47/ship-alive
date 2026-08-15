//! Art post-processing: takes raw generated images from `art_raw/`, removes
//! the solid magenta key background (flood fill from the borders), crops to
//! content, pads to a square with transparency, resizes, and writes PNGs to
//! `assets/art/` where the game picks them up.
//!
//! Usage: cargo run --bin prep_art

use image::{DynamicImage, ImageBuffer, Rgba};
use std::path::Path;

const KEY: Rgba<u8> = Rgba([255, 0, 255, 255]);
const TOLERANCE: i32 = 60;
const OUT_SIZE: u32 = 256;

fn is_key(p: Rgba<u8>) -> bool {
    (p[0] as i32 - KEY[0] as i32).abs() + (p[1] as i32 - KEY[1] as i32).abs() + (p[2] as i32 - KEY[2] as i32).abs()
        <= TOLERANCE * 3
}

/// Flood-fill transparency from the image borders so interior key-colored
/// pixels (e.g. inside a hollow ring) survive.
fn remove_background(img: &mut ImageBuffer<Rgba<u8>, Vec<u8>>) {
    let (w, h) = img.dimensions();
    let mut visited = vec![false; (w * h) as usize];
    let mut stack: Vec<u32> = Vec::new();
    for x in 0..w {
        stack.push(x);
        stack.push(x + (h - 1) * w);
    }
    for y in 0..h {
        stack.push(y * w);
        stack.push(y * w + w - 1);
    }
    while let Some(i) = stack.pop() {
        if visited[i as usize] {
            continue;
        }
        visited[i as usize] = true;
        let p = img.get_pixel(i % w, i / w);
        if is_key(*p) {
            img.put_pixel(i % w, i / w, Rgba([0, 0, 0, 0]));
            let x = i % w;
            let y = i / w;
            if x > 0 {
                stack.push(i - 1);
            }
            if x + 1 < w {
                stack.push(i + 1);
            }
            if y > 0 {
                stack.push(i - w);
            }
            if y + 1 < h {
                stack.push(i + w);
            }
        }
    }
}

fn crop_and_square(img: &mut ImageBuffer<Rgba<u8>, Vec<u8>>) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let (w, h) = img.dimensions();
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0;
    let mut max_y = 0;
    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y)[3] > 8 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if max_x < min_x || max_y < min_y {
        return img.clone();
    }
    let cw = max_x - min_x + 1;
    let ch = max_y - min_y + 1;
    let side = cw.max(ch);
    let mut out = ImageBuffer::from_pixel(side, side, Rgba([0, 0, 0, 0]));
    let ox = (side - cw) / 2;
    let oy = (side - ch) / 2;
    for y in 0..ch {
        for x in 0..cw {
            out.put_pixel(ox + x, oy + y, *img.get_pixel(min_x + x, min_y + y));
        }
    }
    out
}

fn process(name: &str) {
    let src = Path::new("art_raw").join(format!("{name}.png"));
    let dst = Path::new("assets/art").join(format!("{name}.png"));
    let img = match image::open(&src) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("skip {name}: {e}");
            return;
        }
    };
    let mut rgba = img.to_rgba8();
    remove_background(&mut rgba);
    let squared = crop_and_square(&mut rgba);
    let resized = DynamicImage::ImageRgba8(squared).resize_exact(OUT_SIZE, OUT_SIZE, image::imageops::FilterType::Lanczos3);
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    resized.save(&dst).unwrap();
    println!("{} -> {} ok", src.display(), dst.display());
}

fn main() {
    for name in ["floor", "wall", "rack", "crate", "ore", "part", "crew", "ring", "dot"] {
        process(name);
    }
}
