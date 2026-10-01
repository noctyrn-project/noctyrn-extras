//! Procedural texture generator for weapon-skin pattern sheets.
//!
//! Outputs **grayscale, tileable** PNGs: the skin shader samples a fixed-size
//! crop ("stamp") of the sheet and multiplies it by the skin's tint colour,
//! so one grayscale sheet per motif yields every colourway. Values are
//! centred around mid-grey (0.5) so `tint * sheet * 2` leaves the tint
//! unchanged on average, darkens in shadows and brightens in highlights.
//!
//! `--gen-pattern <motif> <out.png> [size]` with motifs:
//!   camo, tiger, scales, carbon, stripes, digital, splinter, hazard, wood

use std::path::Path;

/// Fixed crop the shader takes out of the sheet (0.5 = a quarter of the
/// sheet, upscaled 2x). Motifs bake their own feature scale into the sheet.
pub const STAMP: f32 = 0.5;

pub fn cmd_gen_pattern(motif: &str, out: &str, size: u32) -> Result<(), String> {
    if size < 64 || size > 8192 {
        return Err(format!("size {size} out of range (64..8192)"));
    }
    let rgba = match motif {
        "camo" => sheet(size, camo),
        "tiger" => sheet(size, tiger),
        "scales" => sheet(size, scales),
        "carbon" => sheet(size, carbon),
        "stripes" => sheet(size, stripes),
        "digital" => sheet(size, digital),
        "splinter" => sheet(size, splinter),
        "hazard" => sheet(size, hazard),
        "wood" => sheet(size, wood),
        other => {
            return Err(format!(
                "unknown motif '{other}' (camo, tiger, scales, carbon, stripes, digital, splinter, hazard, wood)"
            ))
        }
    };
    write_png(out, size, size, &rgba)?;
    eprintln!("Wrote {out} ({size}x{size}, motif {motif}, grayscale)");
    Ok(())
}

fn write_png(path: &str, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    Ok(())
}

/// Paint one square sheet: `f(u, v)` returns a grayscale value in 0..1 for a
/// tileable coordinate pair in 0..1.
fn sheet(size: u32, f: fn(f32, f32) -> f32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / size as f32;
            let v = y as f32 / size as f32;
            let value = f(u, v).clamp(0.0, 1.0);
            let g = (value * 255.0) as u8;
            let i = ((y * size + x) * 4) as usize;
            out[i] = g;
            out[i + 1] = g;
            out[i + 2] = g;
            out[i + 3] = 255;
        }
    }
    out
}

// ── tileable noise ───────────────────────────────────────────────────────
fn hash2(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393)
        ^ (y as u32).wrapping_mul(668_265_263)
        ^ seed.wrapping_mul(2_246_822_519);
    h ^= h >> 13;
    h = h.wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0xffffff) as f32 / 0xffffff as f32
}

/// Value noise whose lattice wraps every `period` cells, so the sheet tiles.
fn pnoise(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let xf = x - xi as f32;
    let yf = y - yi as f32;
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let wrap = |i: i32| i.rem_euclid(period.max(1));
    let a = hash2(wrap(xi), wrap(yi), seed);
    let b = hash2(wrap(xi + 1), wrap(yi), seed);
    let c = hash2(wrap(xi), wrap(yi + 1), seed);
    let d = hash2(wrap(xi + 1), wrap(yi + 1), seed);
    let top = a + (b - a) * u;
    let bottom = c + (d - c) * u;
    top + (bottom - top) * v
}

/// Periodic fbm: `period` cells at the base octave, doubling with each one.
fn pfbm(u: f32, v: f32, period: i32, octaves: u32, seed: u32) -> f32 {
    let mut value = 0.0;
    let mut amp = 0.5;
    let mut period = period.max(1);
    for i in 0..octaves {
        value += amp * pnoise(u * period as f32, v * period as f32, period, seed.wrapping_add(i * 131));
        period *= 2;
        amp *= 0.5;
    }
    value
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Positive fractional part (Rust's `f32::fract` keeps the sign, which
/// breaks tiling for negative coordinates).
fn pfrac(x: f32) -> f32 {
    x - x.floor()
}

// ── motifs ───────────────────────────────────────────────────────────────

/// Blobby multi-level camouflage.
fn camo(u: f32, v: f32) -> f32 {
    let n = pfbm(u, v, 6, 5, 11);
    let levels = [0.32, 0.5, 0.7, 0.88];
    let idx = ((n * 3.999) as usize).min(3);
    levels[idx]
}

/// Distorted diagonal tiger stripes. Integer frequencies keep it tileable.
fn tiger(u: f32, v: f32) -> f32 {
    let warp = pfbm(u, v, 4, 3, 23);
    let phase = (u * 3.0 + v * 1.0) * std::f32::consts::TAU + warp * std::f32::consts::TAU;
    if phase.sin().abs() > 0.45 { 0.78 } else { 0.28 }
}

/// Diamond "scale" lattice (armour plating feel). Tileable via integer
/// frequencies in both diagonals.
fn scales(u: f32, v: f32) -> f32 {
    let a = pfrac(u * 10.0 + v * 10.0);
    let b = pfrac(u * 10.0 - v * 10.0);
    let d = (a - 0.5).abs() + (b - 0.5).abs();
    let edge = 1.0 - smoothstep(0.32, 0.5, d);
    let base = 0.42 + 0.06 * pfbm(u, v, 6, 3, 31);
    (base + edge * 0.4).min(1.0)
}

/// Carbon-fibre cross weave.
fn carbon(u: f32, v: f32) -> f32 {
    let a = ((u + v) * 24.0 * std::f32::consts::TAU).sin();
    let b = ((u - v) * 24.0 * std::f32::consts::TAU).sin();
    0.35 + (a * b).abs() * 0.35
}

/// Bold diagonal racing stripes.
fn stripes(u: f32, v: f32) -> f32 {
    let s = (u * 6.0 + v * 2.0).fract();
    if s < 0.5 { 0.3 } else { 0.78 }
}

/// Blocky pixel camouflage at two scales (hashes wrapped to the lattice so
/// the sheet tiles).
fn digital(u: f32, v: f32) -> f32 {
    const COARSE: i32 = 24;
    const FINE: i32 = 48;
    let a = hash2(
        ((u * COARSE as f32).floor() as i32).rem_euclid(COARSE),
        ((v * COARSE as f32).floor() as i32).rem_euclid(COARSE),
        41,
    );
    let b = hash2(
        ((u * FINE as f32).floor() as i32).rem_euclid(FINE),
        ((v * FINE as f32).floor() as i32).rem_euclid(FINE),
        42,
    );
    let n = a * 0.65 + b * 0.35;
    let levels = [0.28, 0.44, 0.6, 0.8];
    levels[((n * 3.999) as usize).min(3)]
}

/// Sharp angular splinter shapes.
fn splinter(u: f32, v: f32) -> f32 {
    let n = pfbm(u, v, 5, 4, 53);
    let ridge = (n - 0.5).abs() * 2.0;
    let sharp = 1.0 - smoothstep(0.15, 0.35, ridge);
    if sharp > 0.5 { 0.3 } else { 0.78 }
}

/// Diagonal hazard bands with a fine inner line pattern.
fn hazard(u: f32, v: f32) -> f32 {
    let band = ((u + v) * 6.0).fract();
    if band < 0.5 {
        let fine = ((u + v) * 60.0).fract();
        if fine < 0.5 { 0.22 } else { 0.4 }
    } else {
        0.85
    }
}

/// Wavy wood grain.
fn wood(u: f32, v: f32) -> f32 {
    let warp = pfbm(u, v, 3, 4, 67);
    let grain = (v * 20.0 * std::f32::consts::TAU + warp * std::f32::consts::TAU).sin().abs();
    let fine = (v * 60.0 * std::f32::consts::TAU + warp * 2.0 * std::f32::consts::TAU)
        .sin()
        .abs()
        * 0.08;
    0.42 + grain * 0.25 + fine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_motifs_generate_grayscale_tiles() {
        for motif in [
            "camo", "tiger", "scales", "carbon", "stripes", "digital", "splinter", "hazard", "wood",
        ] {
            let f = match motif {
                "camo" => camo,
                "tiger" => tiger,
                "scales" => scales,
                "carbon" => carbon,
                "stripes" => stripes,
                "digital" => digital,
                "splinter" => splinter,
                "hazard" => hazard,
                _ => wood,
            };
            let rgba = sheet(128, f);
            assert_eq!(rgba.len(), 128 * 128 * 4, "{motif} size");
            for px in rgba.chunks(4) {
                assert_eq!(px[0], px[1], "{motif} not grayscale");
                assert_eq!(px[1], px[2], "{motif} not grayscale");
                assert_eq!(px[3], 255, "{motif} alpha");
            }
            let sum: u32 = rgba.chunks(4).map(|p| p[0] as u32).sum();
            let avg = sum as f32 / (128 * 128) as f32;
            assert!((40.0..220.0).contains(&avg), "{motif} average {avg}");
        }
    }

    /// Motifs must be seamless: the wrap edges (u=0 vs u=1, v=0 vs v=1)
    /// must evaluate identically, otherwise a stamp crop shows a seam.
    #[test]
    fn motifs_tile_seamlessly() {
        for motif in [
            "camo", "tiger", "scales", "carbon", "stripes", "digital", "splinter", "hazard", "wood",
        ] {
            let f = match motif {
                "camo" => camo,
                "tiger" => tiger,
                "scales" => scales,
                "carbon" => carbon,
                "stripes" => stripes,
                "digital" => digital,
                "splinter" => splinter,
                "hazard" => hazard,
                _ => wood,
            };
            for i in 0..64 {
                let t = i as f32 / 64.0;
                let left = f(0.0, t);
                let right = f(1.0, t);
                assert!(
                    (left - right).abs() < 1e-4,
                    "{motif} horizontal seam at v={t}: {left} vs {right}"
                );
                let top = f(t, 0.0);
                let bottom = f(t, 1.0);
                assert!(
                    (top - bottom).abs() < 1e-4,
                    "{motif} vertical seam at u={t}: {top} vs {bottom}"
                );
            }
        }
    }

    #[test]
    fn noise_is_deterministic_and_periodic() {
        assert_eq!(pnoise(1.5, 2.5, 8, 7), pnoise(1.5, 2.5, 8, 7));
        assert_ne!(pnoise(1.5, 2.5, 8, 7), pnoise(1.5, 2.5, 8, 8));
        // Same lattice cell one period away -> identical value.
        assert!((pnoise(0.25, 0.25, 4, 3) - pnoise(4.25, 0.25, 4, 3)).abs() < 1e-6);
    }
}
