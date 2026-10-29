// All image processing lives here (no Tauri code), so it is easy to test on its own.
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};

const EXT: [&str; 8] = ["jpg", "jpeg", "png", "webp", "tif", "tiff", "gif", "bmp"];
const FONT: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Opts {
    pub format: String, pub quality: f64,
    pub resize: String, pub width: f64, pub height: f64, pub r_switch: bool, pub keep_ratio: bool,
    pub smart_crop: bool, pub smart_fill: bool, pub fill_color: String,
    pub percent: f64, pub pw: f64, pub ph: f64, pub punit: String, pub pdpi: f64,
    pub side_kind: String, pub side: f64, pub filter: String, pub no_enlarge: bool,
    pub crop: CropO, pub canvas: CanvasO,
    pub rotate: f64, pub flip_h: bool, pub flip_v: bool, pub grayscale: bool,
    pub border: f64, pub border_color: String, pub watermark: Wm,
    pub out_dir: Option<String>, pub rename: String, pub suffix: String, pub conflict: String,
}
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct CropO { pub mode: String, pub w: f64, pub h: f64, pub pw: f64, pub ph: f64, pub unit: String, pub dpi: f64,
    pub rw: f64, pub rh: f64, pub sw: bool, pub xy: bool, pub x: f64, pub y: f64 }
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct CanvasO { pub mode: String, pub w: f64, pub h: f64, pub sw: bool, pub pct: f64, pub add: f64, pub bg: String }
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Wm { pub text: String, pub pos: String, pub size: f64, pub opacity: f64 }

#[derive(Serialize)]
pub struct Dim { pub w: u32, pub h: u32, pub bytes: u64 }
#[derive(Serialize)]
pub struct PreviewOut { pub fmt: String, pub url: String, pub orig: Dim, pub res: Dim }
#[derive(Serialize)]
pub struct Fail { pub file: String, pub error: String }
#[derive(Serialize, Default)]
pub struct RunOut { pub done: u32, pub skipped: u32, pub failed: Vec<Fail> }

fn es<E: std::fmt::Display>(e: E) -> String { e.to_string() }
fn is_on(m: &str) -> bool { !m.is_empty() && m != "none" }
fn px(v: f64, unit: &str, dpi: f64) -> u32 { (v / if unit == "cm" { 2.54 } else { 1.0 } * dpi).round().max(1.0) as u32 }
fn hex(s: &str) -> [u8; 3] {
    let s = s.trim_start_matches('#');
    if s.len() == 6 { if let Ok(v) = u32::from_str_radix(s, 16) { return [(v >> 16) as u8, (v >> 8) as u8, v as u8]; } }
    [255, 255, 255]
}
fn solid(w: u32, h: u32, c: [u8; 3]) -> RgbaImage { RgbaImage::from_pixel(w, h, Rgba([c[0], c[1], c[2], 255])) }
fn kernel(s: &str) -> FilterType {
    match s { "nearest" => FilterType::Nearest, "triangle" => FilterType::Triangle, "cubic" => FilterType::CatmullRom, _ => FilterType::Lanczos3 }
}
fn fit_inside(cw: u32, ch: u32, tw: u32, th: u32) -> (u32, u32) {
    let rw = if tw > 0 { tw as f64 / cw as f64 } else { f64::INFINITY };
    let rh = if th > 0 { th as f64 / ch as f64 } else { f64::INFINITY };
    let r = rw.min(rh);
    (((cw as f64 * r).round() as u32).max(1), ((ch as f64 * r).round() as u32).max(1))
}

pub fn expand(paths: Vec<String>) -> Vec<String> {
    fn walk(p: &Path, out: &mut Vec<String>) {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) { let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect(); v.sort(); for c in v { walk(&c, out); } }
        } else if let Some(e) = p.extension().and_then(|e| e.to_str()) {
            if EXT.contains(&e.to_lowercase().as_str()) { out.push(p.to_string_lossy().to_string()); }
        }
    }
    let mut out = vec![];
    for p in paths { walk(Path::new(&p), &mut out); }
    out
}

fn load(path: &str) -> Result<RgbaImage, String> {
    let mut dec = ImageReader::open(path).map_err(es)?.with_guessed_format().map_err(es)?.into_decoder().map_err(es)?;
    let ori = dec.orientation().map_err(es)?;
    let mut img = DynamicImage::from_decoder(dec).map_err(es)?;
    img.apply_orientation(ori); // fixes sideways phone photos
    Ok(img.to_rgba8())
}

fn blend(img: &mut RgbaImage, x: i32, y: i32, c: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 || a <= 0.0 { return; }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let da = p[3] as f32 / 255.0;
    let oa = a + da * (1.0 - a);
    if oa <= 0.0 { return; }
    for i in 0..3 { p[i] = ((c[i] as f32 * a + p[i] as f32 * da * (1.0 - a)) / oa) as u8; }
    p[3] = (oa * 255.0) as u8;
}

fn watermark(img: &mut RgbaImage, wm: &Wm) {
    let Ok(font) = FontRef::try_from_slice(FONT) else { return };
    let (w, h) = (img.width() as f32, img.height() as f32);
    let size = (w.min(h) * wm.size as f32 / 100.0).max(12.0);
    let scale = PxScale::from(size);
    let sf = font.as_scaled(scale);
    let ids: Vec<_> = wm.text.chars().map(|c| sf.glyph_id(c)).collect();
    let mut width = 0.0;
    for (i, id) in ids.iter().enumerate() { width += sf.h_advance(*id); if i > 0 { width += sf.kern(ids[i - 1], *id); } }
    let height = sf.ascent() - sf.descent();
    let (mx, my) = (w * 0.03, h * 0.03);
    let x0 = match wm.pos.as_str() { "tl" | "bl" => mx, "tr" | "br" => w - mx - width, _ => (w - width) / 2.0 };
    let y0 = match wm.pos.as_str() { "tl" | "tr" => my, "bl" | "br" => h - my - height, _ => (h - height) / 2.0 };
    let a = (wm.opacity as f32 / 100.0).clamp(0.0, 1.0);
    for pass in 0..2 {
        let (d, col, al) = if pass == 0 { (size * 0.04, [0u8, 0, 0], a * 0.6) } else { (0.0, [255u8, 255, 255], a) };
        let mut x = x0;
        for (i, id) in ids.iter().enumerate() {
            if i > 0 { x += sf.kern(ids[i - 1], *id); }
            let g = id.with_scale_and_position(scale, point(x + d, y0 + sf.ascent() + d));
            x += sf.h_advance(*id);
            if let Some(og) = font.outline_glyph(g) {
                let bb = og.px_bounds();
                og.draw(|gx, gy, cov| blend(img, bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32, col, cov * al));
            }
        }
    }
}

/// Runs every edit in order: rotate/flip, crop, resize, canvas, effects, border, watermark.
pub fn process(path: &str, o: &Opts) -> Result<(RgbaImage, String, (u32, u32)), String> {
    let mut img = load(path)?;
    let orig = img.dimensions();
    match (o.rotate as i32).rem_euclid(360) {
        90 => img = imageops::rotate90(&img), 180 => img = imageops::rotate180(&img), 270 => img = imageops::rotate270(&img), _ => {}
    }
    if o.flip_h { imageops::flip_horizontal_in_place(&mut img); }
    if o.flip_v { imageops::flip_vertical_in_place(&mut img); }

    // ---- Crop ----
    let c = &o.crop;
    if is_on(&c.mode) {
        let (w, h) = img.dimensions();
        let (mut a, mut b) = if c.mode == "ratio" {
            let r = c.rw.max(1.0) / c.rh.max(1.0);
            if w as f64 / h as f64 > r { ((h as f64 * r).round() as u32, h) } else { (w, (w as f64 / r).round() as u32) }
        } else {
            let (a, b) = if c.mode == "print" { (px(c.pw, &c.unit, c.dpi), px(c.ph, &c.unit, c.dpi)) } else { (c.w as u32, c.h as u32) };
            if c.sw && ((h > w) != (b > a)) { (b, a) } else { (a, b) }
        };
        if a == 0 { a = w; }
        if b == 0 { b = h; }
        let (a, b) = (a.min(w).max(1), b.min(h).max(1));
        let left = if c.xy { (c.x.max(0.0) as u32).min(w - a) } else { (w - a) / 2 };
        let top = if c.xy { (c.y.max(0.0) as u32).min(h - b) } else { (h - b) / 2 };
        img = imageops::crop_imm(&img, left, top, a, b).to_image();
    }

    // ---- Resize ----
    let rm = o.resize.as_str();
    if is_on(rm) {
        let (cw, ch) = img.dimensions();
        let f = kernel(&o.filter);
        let (mut tw, mut th, mut mode) = (0u32, 0u32, "inside");
        match rm {
            "percent" => {
                tw = (cw as f64 * o.percent / 100.0).round().max(1.0) as u32;
                th = (ch as f64 * o.percent / 100.0).round().max(1.0) as u32;
                mode = "fill";
            }
            "side" => {
                let s = o.side.max(1.0) as u32;
                let (k, wide) = (o.side_kind.as_str(), cw >= ch);
                if k == "width" || (k == "long" && wide) || (k == "short" && !wide) { tw = s } else { th = s }
            }
            _ => {
                let (a, b) = if rm == "print" { (px(o.pw, &o.punit, o.pdpi), px(o.ph, &o.punit, o.pdpi)) } else { (o.width as u32, o.height as u32) };
                (tw, th) = if o.r_switch && a > 0 && b > 0 && ((ch > cw) != (b > a)) { (b, a) } else { (a, b) };
                if tw > 0 && th > 0 {
                    mode = if o.smart_crop { "cover" } else if o.smart_fill { "contain" } else if o.keep_ratio { "inside" } else { "fill" };
                }
            }
        }
        if tw > 0 || th > 0 {
            let (iw, ih) = fit_inside(cw, ch, tw, th);
            let enlarging = if mode == "fill" || mode == "cover" { tw > cw || th > ch } else { iw > cw || ih > ch };
            if !(o.no_enlarge && enlarging) {
                img = match mode {
                    "fill" => imageops::resize(&img, tw, th, f),
                    "cover" => {
                        let s = (tw as f64 / cw as f64).max(th as f64 / ch as f64);
                        let (rw, rh) = (((cw as f64 * s).round() as u32).max(tw), ((ch as f64 * s).round() as u32).max(th));
                        let r = imageops::resize(&img, rw, rh, f);
                        imageops::crop_imm(&r, (rw - tw) / 2, (rh - th) / 2, tw, th).to_image()
                    }
                    "contain" => {
                        let r = imageops::resize(&img, iw, ih, f);
                        let mut c = solid(tw, th, hex(&o.fill_color));
                        imageops::overlay(&mut c, &r, ((tw - iw) / 2) as i64, ((th - ih) / 2) as i64);
                        c
                    }
                    _ => imageops::resize(&img, iw, ih, f),
                };
            }
        }
    }

    // ---- Canvas ----
    let v = &o.canvas;
    if is_on(&v.mode) {
        let (cw, ch) = img.dimensions();
        let (mut nw, mut nh) = match v.mode.as_str() {
            "percent" => ((cw as f64 * v.pct / 100.0).round() as u32, (ch as f64 * v.pct / 100.0).round() as u32),
            "expand" => (cw + 2 * v.add.max(0.0) as u32, ch + 2 * v.add.max(0.0) as u32),
            _ => if v.sw && ch > cw { (v.h as u32, v.w as u32) } else { (v.w as u32, v.h as u32) },
        };
        if nw == 0 { nw = cw; }
        if nh == 0 { nh = ch; }
        let mut c = solid(nw, nh, hex(&v.bg));
        imageops::overlay(&mut c, &img, (nw as i64 - cw as i64) / 2, (nh as i64 - ch as i64) / 2);
        img = c;
    }

    // ---- Effects, border, watermark ----
    if o.grayscale { img = DynamicImage::ImageLumaA8(imageops::grayscale_alpha(&img)).to_rgba8(); }
    let b = o.border.max(0.0) as u32;
    if b > 0 {
        let mut c = solid(img.width() + 2 * b, img.height() + 2 * b, hex(&o.border_color));
        imageops::overlay(&mut c, &img, b as i64, b as i64);
        img = c;
    }
    if !o.watermark.text.trim().is_empty() { watermark(&mut img, &o.watermark); }

    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let fmt = if o.format.is_empty() || o.format == "same" { ext } else { o.format.clone() };
    let fmt = match fmt.as_str() { "jpg" | "jpeg" => "jpg", "tif" | "tiff" => "tiff", "png" | "webp" | "gif" | "bmp" => fmt.as_str(), _ => "png" }.to_string();
    Ok((img, fmt, orig))
}

fn flatten(img: &RgbaImage) -> image::RgbImage {
    let mut o = image::RgbImage::new(img.width(), img.height());
    for (x, y, p) in img.enumerate_pixels() {
        let a = p[3] as u32;
        let f = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        o.put_pixel(x, y, image::Rgb([f(p[0]), f(p[1]), f(p[2])]));
    }
    o
}

pub fn encode(img: &RgbaImage, fmt: &str, quality: f64) -> Result<Vec<u8>, String> {
    let mut buf = Cursor::new(Vec::new());
    match fmt {
        "jpg" => DynamicImage::ImageRgb8(flatten(img))
            .write_with_encoder(JpegEncoder::new_with_quality(&mut buf, quality.clamp(1.0, 100.0) as u8)).map_err(es)?,
        "png" => DynamicImage::ImageRgba8(img.clone())
            .write_with_encoder(PngEncoder::new_with_quality(&mut buf, CompressionType::Best, PngFilter::Adaptive)).map_err(es)?,
        "bmp" => DynamicImage::ImageRgb8(flatten(img)).write_to(&mut buf, ImageFormat::Bmp).map_err(es)?,
        "tiff" => DynamicImage::ImageRgba8(img.clone()).write_to(&mut buf, ImageFormat::Tiff).map_err(es)?,
        "gif" => DynamicImage::ImageRgba8(img.clone()).write_to(&mut buf, ImageFormat::Gif).map_err(es)?,
        _ => DynamicImage::ImageRgba8(img.clone()).write_to(&mut buf, ImageFormat::WebP).map_err(es)?,
    }
    Ok(buf.into_inner())
}

pub fn preview(path: &str, o: &Opts) -> Result<PreviewOut, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let (img, fmt, orig) = process(path, o)?;
    let bytes = encode(&img, &fmt, o.quality)?.len() as u64;
    let (w, h) = img.dimensions();
    let small = if w.max(h) > 1000 { let (nw, nh) = fit_inside(w, h, 1000, 1000); imageops::resize(&img, nw, nh, FilterType::Triangle) } else { img };
    let png = encode(&small, "png", 0.0)?;
    Ok(PreviewOut {
        fmt, url: format!("data:image/png;base64,{}", STANDARD.encode(png)),
        orig: Dim { w: orig.0, h: orig.1, bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) },
        res: Dim { w, h, bytes },
    })
}

fn unique(p: PathBuf) -> PathBuf {
    if !p.exists() { return p; }
    let (dir, stem) = (p.parent().map(|d| d.to_path_buf()).unwrap_or_default(), p.file_stem().unwrap_or_default().to_string_lossy().to_string());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..).map(|i| dir.join(format!("{stem}_{i}{ext}"))).find(|c| !c.exists()).unwrap()
}

/// Ok(true) = saved, Ok(false) = skipped.
pub fn convert(path: &str, idx: usize, o: &Opts) -> Result<bool, String> {
    let src = Path::new(path);
    let (img, fmt, _) = process(path, o)?;
    let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let base = if o.rename.trim().is_empty() { format!("{stem}{}", o.suffix) } else {
        let (mut s, mut run) = (String::new(), 0usize);
        for ch in o.rename.trim().chars().chain(std::iter::once('\0')) {
            if ch == '#' { run += 1; continue; }
            if run > 0 { s += &format!("{:0>w$}", idx + 1, w = run); run = 0; }
            if ch != '\0' { s.push(ch); }
        }
        s
    };
    let dir = match &o.out_dir { Some(d) if !d.is_empty() => PathBuf::from(d), _ => src.parent().unwrap_or(Path::new(".")).to_path_buf() };
    std::fs::create_dir_all(&dir).map_err(es)?;
    let mut out = dir.join(format!("{base}.{fmt}"));
    let same = out.exists() && std::fs::canonicalize(&out).ok() == std::fs::canonicalize(src).ok();
    if same && o.conflict != "overwrite" { out = unique(out); }
    else if out.exists() {
        if o.conflict == "skip" { return Ok(false); }
        if o.conflict == "rename" || o.conflict.is_empty() { out = unique(out); }
    }
    std::fs::write(&out, encode(&img, &fmt, o.quality)?).map_err(es)?;
    Ok(true)
}
