//! Bakes Noctyrn brand SVGs into PNGs for the game.
//!
//! The SVGs in `assets/ui/` are the editable source of truth. Production
//! assets are PNGs committed into `../noctyrn-game/assets/ui/`:
//!
//! - `logo.svg`  → `logo.png` (1024x1024, transparent)
//! - `icon.svg`  → `icon.png` (512x512, rounded dark background)
//! - `noctyrn.svg` → `noctyrn.png` (1024x171, stylized wordmark — final
//!   frame of the animation, used on the main menu)
//! - `animation.svg` → `noctyrn_anim_sheet.png` (sprite sheet: 6 cols x 5 rows
//!   of 1024x171 cells = 30 frames of the splash animation)
//!
//! resvg/usvg do not evaluate SMIL animations, so this tool samples the
//! `<animate>`/`<animateTransform>` timeline itself (the file only uses
//! dur=0.5s, keyTimes "0;1", calcMode="spline", keySplines cubic beziers),
//! rewrites each frame into a static SVG, and renders it.

use std::fs;
use std::path::PathBuf;

use image::{Rgba, RgbaImage};

const SVG_DIR: &str = "assets/ui";
const OUT_DIR: &str = "noctyrn-game/assets/ui";

const LOGO_SIZE: u32 = 1024;
const ICON_SIZE: u32 = 512;
const NOCTYRN_W: u32 = 1024;
const NOCTYRN_H: u32 = 171;

// Animation sprite sheet layout.
const ANIM_FRAMES: u32 = 30;
const ANIM_COLS: u32 = 6;
const ANIM_ROWS: u32 = 5;
const ANIM_CELL_W: u32 = 1024;
const ANIM_CELL_H: u32 = 171;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dump_frames = false;
    let mut dump_dir = PathBuf::from("/tmp/opencode/noctyrn-frames");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dump-frames" => {
                dump_frames = true;
                if i + 1 < args.len() {
                    i += 1;
                    dump_dir = PathBuf::from(&args[i]);
                }
            }
            other => panic!("unknown arg: {other}"),
        }
        i += 1;
    }
    if dump_frames {
        fs::create_dir_all(&dump_dir).expect("create dump dir");
    }

    let svg_dir = resolve_svg_dir();
    let out_dir = resolve_out_dir();
    fs::create_dir_all(&out_dir).expect("create output dir");

    let logo_svg = fs::read_to_string(svg_dir.join("logo.svg")).expect("read logo.svg");
    render_static(&logo_svg, LOGO_SIZE, LOGO_SIZE, &out_dir.join("logo.png"));

    let icon_svg = fs::read_to_string(svg_dir.join("icon.svg")).expect("read icon.svg");
    render_static(&icon_svg, ICON_SIZE, ICON_SIZE, &out_dir.join("icon.png"));

    let noctyrn_svg = fs::read_to_string(svg_dir.join("noctyrn.svg")).expect("read noctyrn.svg");
    render_static(&noctyrn_svg, NOCTYRN_W, NOCTYRN_H, &out_dir.join("noctyrn.png"));

    let anim_svg = fs::read_to_string(svg_dir.join("animation.svg")).expect("read animation.svg");
    let sheet = bake_animation(&anim_svg, dump_frames, &dump_dir);
    sheet
        .save(out_dir.join("noctyrn_anim_sheet.png"))
        .expect("save sprite sheet");
    println!(
        "baked: logo.png ({}x{}), icon.png ({}x{}), noctyrn.png ({}x{}), noctyrn_anim_sheet.png ({}x{}, {} frames)",
        LOGO_SIZE,
        LOGO_SIZE,
        ICON_SIZE,
        ICON_SIZE,
        NOCTYRN_W,
        NOCTYRN_H,
        sheet.width(),
        sheet.height(),
        ANIM_FRAMES
    );
}

// ── Static rendering ──

/// Find the SVGs whether the tool is run from `bake/` or the repo root.
fn resolve_svg_dir() -> PathBuf {
    for candidate in [PathBuf::from(SVG_DIR), PathBuf::from("../assets/ui")] {
        if candidate.join("logo.svg").exists() {
            return candidate;
        }
    }
    panic!("cannot find {SVG_DIR}/ — run from the noctyrn-extras repo root or bake/");
}

/// Find the game assets dir whether the tool is run from `bake/` or the repo root.
fn resolve_out_dir() -> PathBuf {
    for candidate in [PathBuf::from(OUT_DIR), PathBuf::from("../../noctyrn-game/assets/ui")] {
        if candidate.exists() || candidate.parent().is_some_and(|p| p.exists()) {
            return candidate;
        }
    }
    panic!("cannot resolve {OUT_DIR}/ — run from the noctyrn-extras repo root or bake/");
}

fn render_static(svg: &str, w: u32, h: u32, out: &PathBuf) {
    let framed = frame_svg(svg, w, h);
    let pixmap = render_svg(&framed);
    let img = RgbaImage::from_raw(w, h, pixmap.data().to_vec()).expect("pixmap size");
    img.save(out).expect("save png");
}

fn render_svg(svg: &str) -> resvg::tiny_skia::Pixmap {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(svg, &opt).expect("parse svg");
    let w = tree.size().width().ceil() as u32;
    let h = tree.size().height().ceil() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).expect("create pixmap");
    resvg::render(&tree, resvg::tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap
}

/// Force an explicit pixel size onto an SVG (replaces width/height attrs).
fn frame_svg(svg: &str, w: u32, h: u32) -> String {
    let doc = roxmltree::Document::parse(svg).expect("parse svg doc");
    let root = doc.root_element();
    let (start, end) = tag_range(root, svg);
    let tag = &svg[start..end];
    let mut patched = String::with_capacity(tag.len() + 32);
    let mut rest = tag;
    while let Some(pos) = rest.find("width=\"") {
        patched.push_str(&rest[..pos]);
        let val_start = pos + "width=\"".len();
        let val_end = rest[val_start..].find('"').map(|o| val_start + o).expect("width quote");
        patched.push_str(&format!("width=\"{w}\""));
        rest = &rest[val_end + 1..];
    }
    patched.push_str(rest);
    rest = patched.as_str();
    let mut final_tag = String::with_capacity(rest.len() + 32);
    while let Some(pos) = rest.find("height=\"") {
        final_tag.push_str(&rest[..pos]);
        let val_start = pos + "height=\"".len();
        let val_end = rest[val_start..].find('"').map(|o| val_start + o).expect("height quote");
        final_tag.push_str(&format!("height=\"{h}\""));
        rest = &rest[val_end + 1..];
    }
    final_tag.push_str(rest);
    format!("{}{}{}", &svg[..start], final_tag, &svg[end..])
}

// ── SMIL sampling ──

/// Duration of the source animation (the SVG's own 0.5s timeline).
const ANIM_SOURCE_DUR: f32 = 0.5;

fn bake_animation(svg: &str, dump_frames: bool, dump_dir: &PathBuf) -> RgbaImage {
    let mut sheet = RgbaImage::from_pixel(
        ANIM_COLS * ANIM_CELL_W,
        ANIM_ROWS * ANIM_CELL_H,
        Rgba([0, 0, 0, 0]),
    );
    for i in 0..ANIM_FRAMES {
        let t = (i as f32 / (ANIM_FRAMES - 1) as f32) * ANIM_SOURCE_DUR;
        let sampled = sample_frame(svg, t);
        let frame_svg = frame_svg(&sampled, ANIM_CELL_W, ANIM_CELL_H);
        let pixmap = render_svg(&frame_svg);
        assert_eq!(
            (pixmap.width(), pixmap.height()),
            (ANIM_CELL_W, ANIM_CELL_H),
            "frame {i} wrong size"
        );
        let col = i % ANIM_COLS;
        let row = i / ANIM_COLS;
        let img = RgbaImage::from_raw(ANIM_CELL_W, ANIM_CELL_H, pixmap.data().to_vec())
            .expect("frame buffer");
        image::imageops::replace(&mut sheet, &img, (col * ANIM_CELL_W) as i64, (row * ANIM_CELL_H) as i64);        if dump_frames {
            img.save(dump_dir.join(format!("frame_{i:02}.png")))
                .expect("dump frame");
        }
    }
    sheet
}

/// Rewrite the SVG so every animated attribute holds its value at time `t`
/// (in source-SVG seconds) and the `<animate>` elements are removed.
fn sample_frame(svg: &str, t: f32) -> String {
    let doc = roxmltree::Document::parse(svg).expect("parse svg doc");
    // (start, end) byte ranges to delete — the animate tags themselves.
    let mut removals: Vec<(usize, usize)> = Vec::new();
    // (start, end, replacement) edits inside parent opening tags.
    let mut patches: Vec<(usize, usize, String)> = Vec::new();

    for node in doc.descendants() {
        if !node.is_element() {
            continue;
        }
        let tag = node.tag_name().name();
        let is_animate = tag == "animate" || tag == "animateTransform";
        if !is_animate {
            continue;
        }
        let attr = node.attribute("attributeName").expect("attributeName");
        let parent = node.parent().expect("animate parent");
        let parent_tag = tag_range(parent, svg);

        let (from, to): (String, String) = if tag == "animate" {
            let to = node.attribute("to").expect("animate to");
            let from = match node.attribute("from") {
                Some(f) => f.to_string(),
                None => current_attr(svg, parent_tag, attr).expect("implicit from"),
            };
            (from, to.to_string())
        } else {
            // animateTransform — this file only uses type="translate".
            (String::from("0 0"), node.attribute("to").expect("animateTransform to").to_string())
        };

        let dur = parse_dur(node.attribute("dur").expect("dur"));
        let splines = parse_key_splines(node.attribute("keySplines"));
        let eased = cubic_bezier_ease((t / dur).clamp(0.0, 1.0), splines);
        let value = interp_attr(attr, &from, &to, eased);

        patches.push(replace_attr(svg, parent_tag, attr, &value));
        removals.push((node.range().start, node.range().end));
    }

    let mut edits: Vec<(usize, usize, Option<String>)> = Vec::new();
    edits.extend(removals.into_iter().map(|(s, e)| (s, e, None)));
    edits.extend(patches.into_iter().map(|(s, e, r)| (s, e, Some(r))));
    edits.sort_by_key(|(s, _, _)| *s);

    let mut out = String::with_capacity(svg.len());
    let mut cursor = 0;
    for (s, e, rep) in edits {
        out.push_str(&svg[cursor..s]);
        if let Some(r) = rep {
            out.push_str(&r);
        }
        cursor = e;
    }
    out.push_str(&svg[cursor..]);
    out
}

/// Byte range of a node's opening tag (start .. first '>').
fn tag_range(node: roxmltree::Node, src: &str) -> (usize, usize) {
    let start = node.range().start;
    let end = src[start..]
        .find('>')
        .map(|o| start + o + 1)
        .expect("tag end");
    (start, end)
}

/// The current value of `attr` in the parent's opening tag (implicit `from`).
fn current_attr(src: &str, tag: (usize, usize), attr: &str) -> Option<String> {
    let tag_src = &src[tag.0..tag.1];
    let needle = format!("{attr}=\"");
    let pos = tag_src.find(&needle)?;
    let rest = &tag_src[pos + needle.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Edit one attribute's value inside a tag; inserts the attribute if missing.
fn replace_attr(src: &str, tag: (usize, usize), attr: &str, value: &str) -> (usize, usize, String) {
    let tag_src = &src[tag.0..tag.1];
    let needle = format!("{attr}=\"");
    if let Some(pos) = tag_src.find(&needle) {
        let val_start = pos + needle.len();
        let val_end = tag_src[val_start..].find('"').map(|o| val_start + o).expect("attr quote");
        (tag.0 + val_start, tag.0 + val_end, value.to_string())
    } else {
        // No such attribute yet → insert before the closing '>'.
        let close = tag_src.len() - 1;
        (tag.0 + close, tag.0 + close, format!(" {attr}=\"{value}\""))
    }
}

fn parse_dur(dur: &str) -> f32 {
    dur.strip_suffix('s')
        .expect("dur in seconds")
        .parse()
        .expect("dur number")
}

fn parse_key_splines(s: Option<&str>) -> [f32; 4] {
    match s {
        None => [0.0, 0.0, 1.0, 1.0], // linear
        Some(v) => {
            let parts: Vec<f32> = v
                .split_whitespace()
                .map(|p| p.parse().expect("keySplines number"))
                .collect();
            assert_eq!(parts.len(), 4, "only one keySpline segment supported");
            [parts[0], parts[1], parts[2], parts[3]]
        }
    }
}

/// CSS cubic-bezier easing: given x in [0,1], solve for the y on the curve
/// defined by control points (x1,y1,x2,y2).
fn cubic_bezier_ease(x: f32, cp: [f32; 4]) -> f32 {
    let [x1, y1, x2, y2] = cp;
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    let mut t = x;
    for _ in 0..32 {
        let xt = bezier_x(t, x1, x2);
        if (xt - x).abs() < 1e-5 {
            break;
        }
        if xt < x {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    bezier_y(t, y1, y2)
}

fn bezier_x(t: f32, x1: f32, x2: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t
}

fn bezier_y(t: f32, y1: f32, y2: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t
}

/// Interpolate between `from` and `to` attribute values at fraction `f`:
/// space-separated numbers ("x,y" pairs are handled by treating commas as
/// separators, so points/translate values interpolate per component).
fn interp_attr(_attr: &str, from: &str, to: &str, f: f32) -> String {
    let from_vals: Vec<f32> = from
        .split(|c: char| c == ' ' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|v| v.parse().expect("value number"))
        .collect();
    let to_vals: Vec<f32> = to
        .split(|c: char| c == ' ' || c == ',')
        .filter(|s| !s.is_empty())
        .map(|v| v.parse().expect("value number"))
        .collect();
    assert_eq!(from_vals.len(), to_vals.len(), "from/to mismatch");
    from_vals
        .iter()
        .zip(to_vals.iter())
        .map(|(a, b)| format!("{:.6}", a + (b - a) * f))
        .collect::<Vec<_>>()
        .join(" ")
}
