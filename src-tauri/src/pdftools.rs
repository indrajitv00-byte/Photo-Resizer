//! PDF merge / split / compress for Photo Resizer.
//! Pure Rust (lopdf + image). No Tauri types in here so it is easy to test.

use image::{imageops::FilterType, DynamicImage, ExtendedColorType, GrayImage, RgbImage};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct PdfInfo {
    pub path: String,
    pub pages: u32,
    pub bytes: u64,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct MergeOut {
    pub pages: u32,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct CompressRes {
    pub file: String,
    pub out: Option<String>,
    pub before: u64,
    pub after: u64,
    pub images: u32,
    pub skipped: bool,
    pub error: Option<String>,
}

// ---------- small helpers ----------

fn file_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

fn stem(p: &str) -> String {
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string())
}

fn load(path: &str) -> Result<Document, String> {
    let doc = Document::load(path).map_err(|e| format!("{}: {}", file_name(path), e))?;
    if doc.trailer.has(b"Encrypt") {
        return Err(format!(
            "{} is password-protected. Remove the password first.",
            file_name(path)
        ));
    }
    Ok(doc)
}

fn out_folder(src: &str, out_dir: Option<&str>) -> PathBuf {
    match out_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => Path::new(src)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default(),
    }
}

/// dir/name.pdf, or dir/name (2).pdf, (3) ... if it already exists.
fn unique_path(dir: &Path, base: &str) -> PathBuf {
    let first = dir.join(format!("{base}.pdf"));
    if !first.exists() {
        return first;
    }
    let mut n = 2u32;
    loop {
        let p = dir.join(format!("{base} ({n}).pdf"));
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

fn name_of(o: &Object) -> Option<Vec<u8>> {
    match o {
        Object::Name(n) => Some(n.clone()),
        _ => None,
    }
}

fn int_of(d: &Dictionary, key: &[u8]) -> Option<i64> {
    match d.get(key).ok()? {
        Object::Integer(i) => Some(*i),
        _ => None,
    }
}

fn name_is(d: &Dictionary, key: &[u8], want: &str) -> bool {
    d.get(key)
        .ok()
        .and_then(name_of)
        .map(|n| n == want.as_bytes())
        .unwrap_or(false)
}

fn filters(d: &Dictionary) -> Vec<Vec<u8>> {
    match d.get(b"Filter").ok() {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(a)) => a.iter().filter_map(name_of).collect(),
        _ => vec![],
    }
}

fn only_filter(f: &[Vec<u8>], name: &str) -> bool {
    f.len() == 1 && f[0] == name.as_bytes()
}

// ---------- info ----------

pub fn info(paths: Vec<String>) -> Vec<PdfInfo> {
    paths
        .into_iter()
        .map(|p| {
            let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            match load(&p) {
                Ok(d) => PdfInfo {
                    pages: d.get_pages().len() as u32,
                    path: p,
                    bytes,
                    error: None,
                },
                Err(e) => PdfInfo {
                    path: p,
                    pages: 0,
                    bytes,
                    error: Some(e),
                },
            }
        })
        .collect()
}

// ---------- merge ----------

/// Pages can inherit Resources / MediaBox / CropBox / Rotate from their parent
/// "Pages" node. Because merging throws the old page tree away, copy those
/// values onto each page first so nothing is lost.
fn flatten_inherited(doc: &mut Document) {
    let keys: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    for pid in pages {
        let page = match doc.get_dictionary(pid) {
            Ok(d) => d.clone(),
            Err(_) => continue,
        };
        let mut found: Vec<(&[u8], Object)> = Vec::new();
        for key in keys {
            if page.has(key) {
                continue;
            }
            let mut node = page.clone();
            for _ in 0..32 {
                let parent_id = match node.get(b"Parent") {
                    Ok(Object::Reference(id)) => *id,
                    _ => break,
                };
                let parent = match doc.get_dictionary(parent_id) {
                    Ok(p) => p,
                    Err(_) => break,
                };
                if let Ok(v) = parent.get(key) {
                    found.push((key, v.clone()));
                    break;
                }
                node = parent.clone();
            }
        }
        if let Some(Object::Dictionary(d)) = doc.objects.get_mut(&pid) {
            for (k, v) in found {
                d.set(k, v);
            }
        }
    }
}

pub fn merge(paths: &[String], out: &str) -> Result<MergeOut, String> {
    if paths.len() < 2 {
        return Err("Add at least two PDFs to merge.".into());
    }
    let mut target = Document::with_version("1.5");
    let mut next_id: u32 = 1;
    let mut kids: Vec<ObjectId> = Vec::new();

    for p in paths {
        let mut doc = load(p)?;
        flatten_inherited(&mut doc);
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;
        // get_pages() is in page order, so the merged order is exactly the list order
        kids.extend(doc.get_pages().into_values());
        for (id, obj) in doc.objects {
            let t = obj.type_name().unwrap_or("");
            if t == "Catalog" || t == "Pages" {
                continue;
            }
            target.objects.insert(id, obj);
        }
    }

    let pages_id: ObjectId = (next_id, 0);
    let catalog_id: ObjectId = (next_id + 1, 0);
    target.max_id = next_id + 1;

    for id in &kids {
        if let Some(Object::Dictionary(d)) = target.objects.get_mut(id) {
            d.set("Parent", Object::Reference(pages_id));
        }
    }

    let mut pd = Dictionary::new();
    pd.set("Type", Object::Name(b"Pages".to_vec()));
    pd.set("Count", Object::Integer(kids.len() as i64));
    pd.set(
        "Kids",
        Object::Array(kids.iter().map(|i| Object::Reference(*i)).collect()),
    );
    target.objects.insert(pages_id, Object::Dictionary(pd));

    let mut cd = Dictionary::new();
    cd.set("Type", Object::Name(b"Catalog".to_vec()));
    cd.set("Pages", Object::Reference(pages_id));
    target.objects.insert(catalog_id, Object::Dictionary(cd));
    target.trailer.set("Root", Object::Reference(catalog_id));

    target.prune_objects();
    target.renumber_objects();
    target.compress();
    target.save(out).map_err(|e| format!("Could not save: {e}"))?;

    Ok(MergeOut {
        pages: kids.len() as u32,
        bytes: std::fs::metadata(out).map(|m| m.len()).unwrap_or(0),
    })
}

// ---------- split ----------

/// "3" -> [3]   "2-5" -> [2,3,4,5]   "7-" -> 7..=total   "-4" -> 1..=4
fn parse_part(tok: &str, total: u32) -> Result<Vec<u32>, String> {
    let t = tok.trim();
    let bad = || format!("“{t}” is not a valid page range.");
    let (a, b) = if let Some((l, r)) = t.split_once('-') {
        let a = if l.trim().is_empty() { 1 } else { l.trim().parse::<u32>().map_err(|_| bad())? };
        let b = if r.trim().is_empty() { total } else { r.trim().parse::<u32>().map_err(|_| bad())? };
        (a, b)
    } else {
        let n = t.parse::<u32>().map_err(|_| bad())?;
        (n, n)
    };
    if a < 1 || b < a || b > total {
        return Err(format!(
            "Pages “{t}” are outside this document (it has {total} pages)."
        ));
    }
    Ok((a..=b).collect())
}

/// [1,2,3,7,9,10] -> "p1-3_7_9-10"
fn label(pages: &[u32]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let start = pages[i];
        let mut end = start;
        while i + 1 < pages.len() && pages[i + 1] == end + 1 {
            i += 1;
            end = pages[i];
        }
        parts.push(if start == end { format!("{start}") } else { format!("{start}-{end}") });
        i += 1;
    }
    format!("p{}", parts.join("_"))
}

pub fn split(
    path: &str,
    mode: &str,
    spec: &str,
    out_dir: Option<&str>,
    progress: &dyn Fn(usize, usize),
) -> Result<Vec<String>, String> {
    let base = load(path)?;
    let total = base.get_pages().len() as u32;
    if total == 0 {
        return Err("This PDF has no pages.".into());
    }

    let groups: Vec<Vec<u32>> = match mode {
        "every" => {
            let n = spec.trim().parse::<u32>().unwrap_or(1).max(1);
            (1..=total)
                .collect::<Vec<u32>>()
                .chunks(n as usize)
                .map(|c| c.to_vec())
                .collect()
        }
        "extract" => {
            let mut set = BTreeSet::new();
            for part in spec.split(',').filter(|s| !s.trim().is_empty()) {
                set.extend(parse_part(part, total)?);
            }
            if set.is_empty() {
                return Err("Type the pages to extract, for example 1-3, 7.".into());
            }
            vec![set.into_iter().collect()]
        }
        _ => {
            let mut v = Vec::new();
            for part in spec.split(',').filter(|s| !s.trim().is_empty()) {
                v.push(parse_part(part, total)?);
            }
            if v.is_empty() {
                return Err("Type the page ranges, for example 1-3, 4-6, 7-.".into());
            }
            v
        }
    };

    let dir = out_folder(path, out_dir);
    let name = stem(path);
    let n = groups.len();
    let mut made = Vec::new();

    for (i, keep) in groups.iter().enumerate() {
        let mut doc = base.clone();
        let drop: Vec<u32> = (1..=total).filter(|p| !keep.contains(p)).collect();
        if !drop.is_empty() {
            doc.delete_pages(&drop);
        }
        doc.prune_objects();
        doc.renumber_objects();
        doc.compress();
        let out = unique_path(&dir, &format!("{}_{}", name, label(keep)));
        doc.save(&out).map_err(|e| format!("Could not save: {e}"))?;
        made.push(out.to_string_lossy().into_owned());
        progress(i + 1, n);
    }
    Ok(made)
}

// ---------- compress ----------

fn level(name: &str) -> (u8, u32) {
    // (JPEG quality, longest side in pixels)
    match name {
        "light" => (80, 2400),
        "strong" => (45, 1100),
        _ => (65, 1700),
    }
}

/// Returns a smaller JPEG (bytes, width, height) for an image stream, or None
/// when the image is of a kind we leave alone.
fn recompress(s: &Stream, quality: u8, max_side: u32) -> Option<(Vec<u8>, u32, u32)> {
    let d = &s.dict;
    if !name_is(d, b"Subtype", "Image") {
        return None;
    }
    if d.has(b"Mask") || d.has(b"Decode") || matches!(d.get(b"ImageMask"), Ok(Object::Boolean(true))) {
        return None;
    }
    let w = u32::try_from(int_of(d, b"Width")?).ok()?;
    let h = u32::try_from(int_of(d, b"Height")?).ok()?;
    if w * h < 20_000 {
        return None; // tiny images are not worth touching
    }
    let f = filters(d);

    let img: DynamicImage = if only_filter(&f, "DCTDecode") {
        image::load_from_memory_with_format(&s.content, image::ImageFormat::Jpeg).ok()?
    } else if only_filter(&f, "FlateDecode") {
        if d.has(b"DecodeParms") || int_of(d, b"BitsPerComponent")? != 8 {
            return None;
        }
        let comps: u32 = if name_is(d, b"ColorSpace", "DeviceRGB") {
            3
        } else if name_is(d, b"ColorSpace", "DeviceGray") {
            1
        } else {
            return None;
        };
        let mut raw = Vec::with_capacity((w * h * comps) as usize);
        flate2::read::ZlibDecoder::new(&s.content[..])
            .read_to_end(&mut raw)
            .ok()?;
        if raw.len() != (w * h * comps) as usize {
            return None;
        }
        if comps == 3 {
            DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, raw)?)
        } else {
            DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, raw)?)
        }
    } else {
        return None; // CCITT / JBIG2 / JPX / LZW etc. are left as they are
    };

    let longest = w.max(h);
    let (nw, nh) = if longest > max_side {
        let k = max_side as f64 / longest as f64;
        (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
    } else {
        (w, h)
    };
    let img = if (nw, nh) != (w, h) {
        img.resize_exact(nw, nh, FilterType::Lanczos3)
    } else {
        img
    };

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        match &img {
            DynamicImage::ImageLuma8(g) => enc.encode(g.as_raw(), nw, nh, ExtendedColorType::L8).ok()?,
            other => {
                let rgb = other.to_rgb8();
                enc.encode(rgb.as_raw(), nw, nh, ExtendedColorType::Rgb8).ok()?
            }
        }
    }
    Some((buf, nw, nh))
}

fn compress_one(path: &str, lvl: &str, out_dir: Option<&str>) -> CompressRes {
    let before = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let fail = |e: String| CompressRes {
        file: path.to_string(),
        out: None,
        before,
        after: before,
        images: 0,
        skipped: false,
        error: Some(e),
    };
    let mut doc = match load(path) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let (q, max_side) = level(lvl);
    let mut images = 0u32;
    for (_, obj) in doc.objects.iter_mut() {
        if let Object::Stream(s) = obj {
            if let Some((buf, nw, nh)) = recompress(s, q, max_side) {
                if buf.len() < s.content.len() {
                    s.dict.set("Width", Object::Integer(nw as i64));
                    s.dict.set("Height", Object::Integer(nh as i64));
                    s.dict.set("BitsPerComponent", Object::Integer(8));
                    s.dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
                    s.dict.remove(b"DecodeParms");
                    s.set_content(buf);
                    images += 1;
                }
            }
        }
    }
    doc.prune_objects();
    doc.delete_zero_length_streams();
    doc.compress();

    let out = unique_path(&out_folder(path, out_dir), &format!("{}_compressed", stem(path)));
    if let Err(e) = doc.save(&out) {
        return fail(format!("Could not save: {e}"));
    }
    let after = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(before);
    if (after as f64) > (before as f64) * 0.98 {
        let _ = std::fs::remove_file(&out);
        return CompressRes {
            file: path.to_string(),
            out: None,
            before,
            after: before,
            images,
            skipped: true,
            error: None,
        };
    }
    CompressRes {
        file: path.to_string(),
        out: Some(out.to_string_lossy().into_owned()),
        before,
        after,
        images,
        skipped: false,
        error: None,
    }
}

pub fn compress(
    files: &[String],
    lvl: &str,
    out_dir: Option<&str>,
    progress: &dyn Fn(usize, usize),
) -> Vec<CompressRes> {
    let n = files.len();
    files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let r = compress_one(f, lvl, out_dir);
            progress(i + 1, n);
            r
        })
        .collect()
}