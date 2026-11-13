//! Core logic for removing empty pages from PDF files.
//!
//! A page is considered empty when it does not draw any visible text and does
//! not draw any meaningful image. Vector graphics are treated as content by
//! default, but can optionally be ignored.
//!
//! Scanned pages are images, so "no text and no image" would keep every blank
//! scan. To handle those, the images actually drawn on a page can be decoded
//! and checked for visible ink. An image that is almost entirely uniform paper
//! background counts as blank.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_FORM_DEPTH: usize = 16;
const MAX_WARNINGS: usize = 12;

/// Longest side used when analysing a page image.
const ANALYSIS_MAX_SIDE: u32 = 1200;
/// Border ignored while looking for ink, as a fraction of the shorter side.
const ANALYSIS_MARGIN: f32 = 0.02;
/// Ink coverage (fraction of non-border pixels) below which an image is blank.
const ANALYSIS_INK_THRESHOLD: f64 = 0.0005;
/// Pages with less ink than this may still be blank if the ink only forms a
/// narrow strip at the page edge (a typical scanner/show-through artifact).
const ANALYSIS_STRIP_INK: f64 = 0.005;
/// Resolution of the coarse grid used to locate ink on a page.
const ANALYSIS_GRID: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct CleanOptions {
    /// Also remove pages whose only content is vector graphics (lines, shapes).
    pub remove_graphics_only_pages: bool,
    /// Decode images drawn on a page and treat a blank scan as empty.
    pub analyze_scanned_pages: bool,
}

impl Default for CleanOptions {
    fn default() -> Self {
        Self {
            remove_graphics_only_pages: false,
            analyze_scanned_pages: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CleanReport {
    pub source: PathBuf,
    pub output: Option<PathBuf>,
    pub total_pages: u32,
    pub removed_pages: Vec<u32>,
    pub overwrote: bool,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

impl CleanReport {
    pub fn kept_pages(&self) -> u32 {
        self.total_pages
            .saturating_sub(self.removed_pages.len() as u32)
    }

    pub fn removed_count(&self) -> usize {
        self.removed_pages.len()
    }
}

pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

pub fn cleaned_output_path(source: &Path) -> PathBuf {
    let mut name = source
        .file_stem()
        .map(|stem| stem.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("document"));
    name.push("-cleaned.pdf");
    source.with_file_name(name)
}

/// Remove empty pages from `source` and write the result next to it as
/// `<name>-cleaned.pdf`.
pub fn clean_pdf(source: &Path, options: CleanOptions) -> CleanReport {
    let mut report = CleanReport {
        source: source.to_path_buf(),
        ..CleanReport::default()
    };

    if !source.is_file() {
        report.error = Some("File not found.".to_string());
        return report;
    }
    if !is_pdf_path(source) {
        report.error = Some("Not a PDF file.".to_string());
        return report;
    }

    let mut doc = match Document::load(source) {
        Ok(doc) => doc,
        Err(err) => {
            report.error = Some(format!("Could not open PDF: {err}"));
            return report;
        }
    };

    if doc.is_encrypted() {
        if let Err(err) = doc.decrypt("") {
            report.error = Some(format!(
                "PDF is password protected and cannot be opened without a password ({err})."
            ));
            return report;
        }
    }

    let pages = doc.get_pages();
    report.total_pages = pages.len() as u32;
    if pages.is_empty() {
        report.error = Some("PDF has no pages.".to_string());
        return report;
    }

    let mut removed_pages: Vec<u32> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut image_cache: HashMap<ObjectId, ImageContent> = HashMap::new();

    for (number, page_id) in &pages {
        let flags = page_flags(
            &doc,
            *page_id,
            *number,
            options,
            &mut image_cache,
            &mut warnings,
        );
        let empty =
            !flags.text && !flags.image && (!flags.drawing || options.remove_graphics_only_pages);
        if empty {
            removed_pages.push(*number);
        }
    }
    report.removed_pages = removed_pages.clone();
    report.warnings = warnings;

    if removed_pages.len() == pages.len() {
        report.error =
            Some("Every page looks empty. Refusing to write a PDF without pages.".to_string());
        report.removed_pages.clear();
        return report;
    }

    let output = cleaned_output_path(source);
    report.overwrote = output.exists();

    if !removed_pages.is_empty() {
        doc.delete_pages(&removed_pages);
        doc.prune_objects();
        doc.renumber_objects();
        doc.compress();
    }

    match write_document(&mut doc, &output) {
        Ok(()) => report.output = Some(output),
        Err(err) => report.error = Some(err),
    }

    report
}

/// Like [`clean_pdf`], but catches unexpected panics from the PDF parser so a
/// single malformed file cannot abort a batch.
pub fn clean_pdf_guarded(source: &Path, options: CleanOptions) -> CleanReport {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clean_pdf(source, options)))
        .unwrap_or_else(|_| CleanReport {
            source: source.to_path_buf(),
            error: Some("Unexpected error while reading this PDF.".to_string()),
            ..CleanReport::default()
        })
}

fn write_document(doc: &mut Document, output: &Path) -> Result<(), String> {
    let file_name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output.pdf".to_string());
    let tmp = output.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));

    let save_result = doc
        .save(&tmp)
        .map_err(|err| format!("Could not write PDF: {err}"));
    if let Err(err) = save_result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }

    if output.exists() {
        fs::remove_file(output).map_err(|err| {
            let _ = fs::remove_file(&tmp);
            format!("Could not replace existing file: {err}")
        })?;
    }

    fs::rename(&tmp, output).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        format!("Could not save cleaned PDF: {err}")
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct ContentFlags {
    text: bool,
    image: bool,
    drawing: bool,
}

impl ContentFlags {
    fn mark_all(&mut self) {
        self.text = true;
        self.image = true;
        self.drawing = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageContent {
    /// The image contains visible ink (or an unsupported encoding that we do
    /// not want to risk removing). Either way the page must be kept.
    Content,
    /// The image is effectively blank paper.
    Blank,
    /// The image could not be decoded or is not safe to judge.
    Unknown,
}

fn page_flags(
    doc: &Document,
    page_id: ObjectId,
    page_number: u32,
    options: CleanOptions,
    image_cache: &mut HashMap<ObjectId, ImageContent>,
    warnings: &mut Vec<String>,
) -> ContentFlags {
    let mut scanner = Scanner {
        doc,
        options,
        image_cache,
    };
    let mut flags = ContentFlags::default();
    let resources = effective_resources(doc, page_id);
    let mut form_path = HashSet::new();

    for stream_id in doc.get_page_contents(page_id) {
        let stream = match doc.get_object(stream_id).and_then(Object::as_stream) {
            Ok(stream) => stream,
            Err(_) => {
                push_warning(
                    warnings,
                    format!("Page {page_number}: content stream is unreadable; page kept."),
                );
                flags.mark_all();
                continue;
            }
        };

        match stream.get_plain_content() {
            Ok(data) => scanner.scan_stream(&data, resources, &mut flags, &mut form_path, 0),
            Err(err) => {
                push_warning(
                    warnings,
                    format!("Page {page_number}: content could not be decoded ({err}); page kept."),
                );
                flags.mark_all();
            }
        }

        if flags.text || flags.image {
            return flags;
        }
    }

    scanner.scan_annotations(
        page_id,
        resources,
        &mut flags,
        &mut form_path,
        page_number,
        warnings,
    );
    flags
}

fn push_warning(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(message);
    }
}

struct Scanner<'a> {
    doc: &'a Document,
    options: CleanOptions,
    image_cache: &'a mut HashMap<ObjectId, ImageContent>,
}

impl<'a> Scanner<'a> {
    fn scan_stream(
        &mut self,
        data: &[u8],
        resources: Option<&'a Dictionary>,
        flags: &mut ContentFlags,
        form_path: &mut HashSet<ObjectId>,
        depth: usize,
    ) {
        if depth > MAX_FORM_DEPTH {
            flags.mark_all();
            return;
        }

        let mut i = 0usize;
        let mut last_name: Option<Vec<u8>> = None;
        let mut last_number: Option<f64> = None;
        let mut text_mode: i32 = 0;
        let mut text_mode_stack: Vec<i32> = Vec::new();

        while i < data.len() {
            let byte = data[i];
            match byte {
                b if is_white(b) => i += 1,
                b'%' => {
                    while i < data.len() && !matches!(data[i], b'\n' | b'\r') {
                        i += 1;
                    }
                }
                b'(' => skip_literal_string(data, &mut i),
                b'<' => {
                    if i + 1 < data.len() && data[i + 1] == b'<' {
                        i += 2;
                    } else {
                        skip_hex_string(data, &mut i);
                    }
                }
                b'>' => {
                    if i + 1 < data.len() && data[i + 1] == b'>' {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                b'[' | b']' | b'{' | b'}' | b')' => i += 1,
                b'/' => {
                    i += 1;
                    let start = i;
                    while i < data.len() && is_regular(data[i]) {
                        i += 1;
                    }
                    last_name = Some(decode_name(&data[start..i]));
                    last_number = None;
                }
                b if b == b'+' || b == b'-' || b == b'.' || b.is_ascii_digit() => {
                    let start = i;
                    i += 1;
                    while i < data.len()
                        && (data[i].is_ascii_digit()
                            || matches!(data[i], b'.' | b'+' | b'-' | b'e' | b'E'))
                    {
                        i += 1;
                    }
                    last_number = std::str::from_utf8(&data[start..i])
                        .ok()
                        .and_then(|raw| raw.parse::<f64>().ok());
                    last_name = None;
                }
                _ => {
                    let start = i;
                    while i < data.len() && is_regular(data[i]) {
                        i += 1;
                    }
                    if start == i {
                        i += 1;
                        continue;
                    }
                    let op = &data[start..i];
                    match op {
                        b"Tj" | b"TJ" | b"'" | b"\"" => {
                            if !is_invisible_text_mode(text_mode) {
                                flags.text = true;
                            }
                        }
                        b"Tr" => {
                            if let Some(mode) = last_number {
                                text_mode = mode as i32;
                            }
                        }
                        b"q" => text_mode_stack.push(text_mode),
                        b"Q" => {
                            if let Some(mode) = text_mode_stack.pop() {
                                text_mode = mode;
                            }
                        }
                        b"Do" => {
                            if let Some(name) = last_name.as_deref() {
                                self.invoke_xobject(name, resources, flags, form_path, depth);
                            }
                        }
                        b"sh" => flags.drawing = true,
                        b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                            flags.drawing = true;
                        }
                        b"BI" => flags.image = true,
                        _ => {}
                    }
                    last_name = None;
                    last_number = None;
                }
            }
        }
    }

    fn invoke_xobject(
        &mut self,
        name: &[u8],
        resources: Option<&'a Dictionary>,
        flags: &mut ContentFlags,
        form_path: &mut HashSet<ObjectId>,
        depth: usize,
    ) {
        let Some(resources) = resources else {
            flags.mark_all();
            return;
        };
        let Some(xobjects) = resources
            .get(b"XObject")
            .ok()
            .and_then(|obj| deref_dict(self.doc, obj))
        else {
            flags.mark_all();
            return;
        };
        let Ok(object) = xobjects.get(name) else {
            flags.mark_all();
            return;
        };

        let (object_id, stream) = match object {
            Object::Reference(id) => match self.doc.get_object(*id).and_then(Object::as_stream) {
                Ok(stream) => (Some(*id), stream),
                Err(_) => {
                    flags.mark_all();
                    return;
                }
            },
            Object::Stream(stream) => (None, stream),
            _ => {
                flags.mark_all();
                return;
            }
        };

        let subtype = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or(b"");
        match subtype {
            b"Image" => {
                if !self.options.analyze_scanned_pages {
                    flags.image = true;
                    return;
                }
                let verdict = match object_id {
                    Some(id) => match self.image_cache.get(&id) {
                        Some(verdict) => *verdict,
                        None => {
                            let verdict = analyze_image(self.doc, stream);
                            self.image_cache.insert(id, verdict);
                            verdict
                        }
                    },
                    None => analyze_image(self.doc, stream),
                };
                if verdict != ImageContent::Blank {
                    flags.image = true;
                }
            }
            b"Form" => {
                let form_resources = stream
                    .dict
                    .get(b"Resources")
                    .ok()
                    .and_then(|obj| deref_dict(self.doc, obj))
                    .or(Some(resources));
                let data = stream.get_plain_content();

                if let Some(id) = object_id {
                    if !form_path.insert(id) {
                        return;
                    }
                }
                match data {
                    Ok(data) => {
                        self.scan_stream(&data, form_resources, flags, form_path, depth + 1)
                    }
                    Err(_) => flags.mark_all(),
                }
                if let Some(id) = object_id {
                    form_path.remove(&id);
                }
            }
            _ => flags.drawing = true,
        }
    }

    fn scan_annotations(
        &mut self,
        page_id: ObjectId,
        resources: Option<&'a Dictionary>,
        flags: &mut ContentFlags,
        form_path: &mut HashSet<ObjectId>,
        page_number: u32,
        warnings: &mut Vec<String>,
    ) {
        let Ok(page) = self.doc.get_dictionary(page_id) else {
            return;
        };
        let Ok(annots) = page.get(b"Annots") else {
            return;
        };
        let Some(Object::Array(annots)) = deref(self.doc, annots) else {
            return;
        };

        for annot_object in annots {
            let Some(annot) = deref_dict(self.doc, annot_object) else {
                continue;
            };
            let Ok(ap_object) = annot.get(b"AP") else {
                continue;
            };
            let Some(ap) = deref_dict(self.doc, ap_object) else {
                continue;
            };
            let Ok(appearance) = ap.get(b"N") else {
                continue;
            };

            match deref(self.doc, appearance) {
                Some(Object::Stream(stream)) => {
                    self.scan_appearance(
                        stream,
                        resources,
                        flags,
                        form_path,
                        page_number,
                        warnings,
                    );
                }
                Some(Object::Dictionary(states)) => {
                    for (_, value) in states.iter() {
                        if let Some(Object::Stream(stream)) = deref(self.doc, value) {
                            self.scan_appearance(
                                stream,
                                resources,
                                flags,
                                form_path,
                                page_number,
                                warnings,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn scan_appearance(
        &mut self,
        stream: &'a Stream,
        inherited_resources: Option<&'a Dictionary>,
        flags: &mut ContentFlags,
        form_path: &mut HashSet<ObjectId>,
        page_number: u32,
        warnings: &mut Vec<String>,
    ) {
        let resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|obj| deref_dict(self.doc, obj))
            .or(inherited_resources);

        match stream.get_plain_content() {
            Ok(data) => self.scan_stream(&data, resources, flags, form_path, 1),
            Err(_) => {
                push_warning(
                    warnings,
                    format!("Page {page_number}: annotation appearance is unreadable; page kept."),
                );
                flags.mark_all();
            }
        }
    }
}

/// Decide whether an image XObject contains visible ink.
///
/// Only common scan encodings are decoded (JPEG and raw/Flate RGB, gray and
/// CMYK). Anything else is reported as unknown so the page is kept.
fn analyze_image(doc: &Document, stream: &Stream) -> ImageContent {
    if dict_bool(doc, &stream.dict, b"ImageMask").unwrap_or(false) {
        // Stencil masks are small decorations; do not risk removing the page.
        return ImageContent::Unknown;
    }

    let filters = if stream.dict.get(b"Filter").is_err() {
        Vec::new()
    } else {
        match stream.filters() {
            Ok(filters) => filters,
            Err(_) => return ImageContent::Unknown,
        }
    };

    if filters.len() == 1 && filters[0] == "DCTDecode" {
        let decoded = match decode_array(doc, &stream.dict) {
            Some(decoded) => decoded,
            None => return ImageContent::Unknown,
        };
        if let Some(pairs) = &decoded {
            if !pairs.iter().all(|&(low, high)| low == 0.0 && high == 1.0) {
                return ImageContent::Unknown;
            }
        }
        return match image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg)
        {
            Ok(image) => classify_image(&image.to_luma8()),
            Err(_) => ImageContent::Unknown,
        };
    }

    let Some(width) = dict_int(doc, &stream.dict, b"Width") else {
        return ImageContent::Unknown;
    };
    let Some(height) = dict_int(doc, &stream.dict, b"Height") else {
        return ImageContent::Unknown;
    };
    if !(1..=20000).contains(&width) || !(1..=20000).contains(&height) {
        return ImageContent::Unknown;
    }
    let pixel_count = width as u64 * height as u64;
    if pixel_count > 50_000_000 {
        return ImageContent::Unknown;
    }

    let bpc = dict_int(doc, &stream.dict, b"BitsPerComponent").unwrap_or(8);
    if !matches!(bpc, 1 | 2 | 4 | 8) {
        return ImageContent::Unknown;
    }

    let Some(color_space) = stream
        .dict
        .get(b"ColorSpace")
        .ok()
        .and_then(|object| resolve_color_space(doc, object))
    else {
        return ImageContent::Unknown;
    };

    if let Ok(params) = stream.dict.get(b"DecodeParms") {
        match deref(doc, params) {
            Some(Object::Dictionary(params)) => {
                if dict_int(doc, params, b"Predictor").unwrap_or(1) > 1 {
                    return ImageContent::Unknown;
                }
            }
            Some(Object::Null) | None => {}
            _ => return ImageContent::Unknown,
        }
    }

    let raw = match filters.as_slice() {
        [] => stream.content.clone(),
        [filter] if filter == "FlateDecode" => match inflate_zlib(&stream.content) {
            Some(data) => data,
            None => return ImageContent::Unknown,
        },
        _ => return ImageContent::Unknown,
    };

    let components = color_space.components();
    let row_bytes = (width as usize * components * bpc as usize).div_ceil(8);
    if raw.len() < row_bytes.saturating_mul(height as usize) {
        return ImageContent::Unknown;
    }

    let decoded = match decode_array(doc, &stream.dict) {
        Some(decoded) => decoded,
        None => return ImageContent::Unknown,
    };
    let decode_pairs = match &decoded {
        None => None,
        Some(pairs) => {
            if matches!(&color_space, ColorSpace::Indexed { .. }) || pairs.len() != components {
                return ImageContent::Unknown;
            }
            Some(pairs.as_slice())
        }
    };

    let max_value = (1u32 << bpc) - 1;
    let mask = max_value as u8;
    let mut gray = vec![0u8; pixel_count as usize];

    for y in 0..height as usize {
        let row_start = y * row_bytes;
        for x in 0..width as usize {
            let mut samples = [0u32; 4];
            for (component, sample) in samples.iter_mut().enumerate().take(components) {
                let sample_index = x * components + component;
                let bit = sample_index * bpc as usize;
                let byte = raw[row_start + bit / 8];
                let shift = 8 - bpc as usize - (bit % 8);
                *sample = ((byte >> shift) & mask) as u32;
            }

            let luma = match &color_space {
                ColorSpace::Gray => scale_components(&samples, max_value, 1, decode_pairs)[0],
                ColorSpace::Rgb => rgb_luma(scale_components(&samples, max_value, 3, decode_pairs)),
                ColorSpace::Cmyk => {
                    cmyk_luma(scale_components(&samples, max_value, 4, decode_pairs))
                }
                ColorSpace::Indexed { base, lookup, high } => {
                    let index = samples[0] as usize;
                    if index > *high {
                        return ImageContent::Unknown;
                    }
                    let base_components = base.components();
                    let offset = index * base_components;
                    let end = offset + base_components;
                    if end > lookup.len() {
                        return ImageContent::Unknown;
                    }
                    indexed_luma(base, &lookup[offset..end]).unwrap_or(255)
                }
            };

            gray[y * width as usize + x] = luma;
        }
    }

    match image::GrayImage::from_raw(width as u32, height as u32, gray) {
        Some(image) => classify_image(&image),
        None => ImageContent::Unknown,
    }
}

/// Decide whether an already decoded grayscale image is blank.
fn classify_image(image: &image::GrayImage) -> ImageContent {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return ImageContent::Unknown;
    }

    let longest = width.max(height);
    let scaled;
    let image = if longest > ANALYSIS_MAX_SIDE {
        let scale = ANALYSIS_MAX_SIDE as f32 / longest as f32;
        let target_width = ((width as f32 * scale).round() as u32).max(1);
        let target_height = ((height as f32 * scale).round() as u32).max(1);
        scaled = image::imageops::thumbnail(image, target_width, target_height);
        &scaled
    } else {
        image
    };

    let (width, height) = image.dimensions();
    let margin = ((width.min(height) as f32) * ANALYSIS_MARGIN).round() as u32;
    let margin = margin
        .min(width.saturating_sub(1) / 2)
        .min(height.saturating_sub(1) / 2);
    let x_start = margin;
    let x_end = width.saturating_sub(margin);
    let y_start = margin;
    let y_end = height.saturating_sub(margin);
    if x_start >= x_end || y_start >= y_end {
        return ImageContent::Unknown;
    }

    let mut histogram = [0u64; 256];
    for y in y_start..y_end {
        for x in x_start..x_end {
            histogram[image.get_pixel(x, y)[0] as usize] += 1;
        }
    }
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return ImageContent::Unknown;
    }

    let mut cumulative = 0u64;
    let mut background = 255u8;
    for (value, count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative * 10 >= total * 9 {
            background = value as u8;
            break;
        }
    }

    // A very dark "background" means this is not a normal paper scan; do not
    // guess, keep the page.
    if background < 128 {
        return ImageContent::Unknown;
    }

    let cut = ((background as u32 * 3) / 4).min(150) as usize;
    let ink: u64 = histogram[..cut].iter().sum();
    let ink_fraction = ink as f64 / total as f64;
    if ink_fraction < ANALYSIS_INK_THRESHOLD {
        return ImageContent::Blank;
    }

    // A little ink is normal on an otherwise blank scan: bleed-through or the
    // footer of the facing page often shows up as one narrow strip at an edge.
    if ink_fraction < ANALYSIS_STRIP_INK
        && is_edge_strip(image, x_start, x_end, y_start, y_end, cut as u8)
    {
        return ImageContent::Blank;
    }

    ImageContent::Content
}

/// Returns true when the ink forms a narrow band that spans most of the page
/// and sits against one of its edges.
fn is_edge_strip(
    image: &image::GrayImage,
    x_start: u32,
    x_end: u32,
    y_start: u32,
    y_end: u32,
    cut: u8,
) -> bool {
    let crop_width = (x_end - x_start) as usize;
    let crop_height = (y_end - y_start) as usize;
    if crop_width == 0 || crop_height == 0 {
        return false;
    }

    let mut cells = [[0u16; ANALYSIS_GRID]; ANALYSIS_GRID];
    for y in y_start..y_end {
        let grid_y = ((y - y_start) as usize * ANALYSIS_GRID / crop_height).min(ANALYSIS_GRID - 1);
        for x in x_start..x_end {
            if image.get_pixel(x, y)[0] < cut {
                let grid_x =
                    ((x - x_start) as usize * ANALYSIS_GRID / crop_width).min(ANALYSIS_GRID - 1);
                cells[grid_y][grid_x] = cells[grid_y][grid_x].saturating_add(1);
            }
        }
    }

    let (mut left, mut right, mut top, mut bottom) = (ANALYSIS_GRID, 0usize, ANALYSIS_GRID, 0usize);
    for (grid_y, row) in cells.iter().enumerate() {
        for (grid_x, &count) in row.iter().enumerate() {
            if count >= 2 {
                left = left.min(grid_x);
                right = right.max(grid_x);
                top = top.min(grid_y);
                bottom = bottom.max(grid_y);
            }
        }
    }
    if left > right || top > bottom {
        // Only isolated dust: still blank.
        return true;
    }

    let width = (right - left + 1) as f32 / ANALYSIS_GRID as f32;
    let height = (bottom - top + 1) as f32 / ANALYSIS_GRID as f32;
    let near_left = left as f32 / ANALYSIS_GRID as f32 <= 0.25;
    let near_right = (right + 1) as f32 / ANALYSIS_GRID as f32 >= 0.75;
    let near_top = top as f32 / ANALYSIS_GRID as f32 <= 0.25;
    let near_bottom = (bottom + 1) as f32 / ANALYSIS_GRID as f32 >= 0.75;

    let vertical_strip = width <= 0.15 && height >= 0.5 && (near_left || near_right);
    let horizontal_strip = height <= 0.15 && width >= 0.5 && (near_top || near_bottom);
    vertical_strip || horizontal_strip
}

#[derive(Debug, Clone)]
enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Indexed {
        base: Box<ColorSpace>,
        lookup: Vec<u8>,
        high: usize,
    },
}

impl ColorSpace {
    fn components(&self) -> usize {
        match self {
            ColorSpace::Gray => 1,
            ColorSpace::Rgb => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Indexed { .. } => 1,
        }
    }
}

fn resolve_color_space(doc: &Document, object: &Object) -> Option<ColorSpace> {
    match deref(doc, object)? {
        Object::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Some(ColorSpace::Gray),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorSpace::Rgb),
            b"DeviceCMYK" | b"CMYK" => Some(ColorSpace::Cmyk),
            _ => None,
        },
        Object::Array(items) => {
            let family = deref(doc, items.first()?)?.as_name().ok()?;
            match family {
                b"Indexed" | b"I" => {
                    let base = resolve_color_space(doc, items.get(1)?)?;
                    if matches!(&base, ColorSpace::Indexed { .. }) {
                        return None;
                    }
                    let high = deref(doc, items.get(2)?)?.as_i64().ok()?;
                    if high < 0 {
                        return None;
                    }
                    let lookup_object = deref(doc, items.get(3)?)?;
                    let lookup = match lookup_object {
                        Object::String(bytes, _) => bytes.clone(),
                        Object::Stream(stream) => stream.get_plain_content().ok()?,
                        _ => return None,
                    };
                    Some(ColorSpace::Indexed {
                        base: Box::new(base),
                        lookup,
                        high: high as usize,
                    })
                }
                b"ICCBased" => {
                    let stream = deref(doc, items.get(1)?)?.as_stream().ok()?;
                    let components = stream
                        .dict
                        .get(b"N")
                        .ok()
                        .and_then(|object| deref(doc, object))
                        .and_then(|object| object.as_i64().ok())?;
                    match components {
                        1 => Some(ColorSpace::Gray),
                        3 => Some(ColorSpace::Rgb),
                        4 => Some(ColorSpace::Cmyk),
                        _ => None,
                    }
                }
                b"CalGray" => Some(ColorSpace::Gray),
                b"CalRGB" => Some(ColorSpace::Rgb),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Read the optional `/Decode` array. `Some(None)` means the key is absent,
/// `None` means the array is malformed (caller keeps the page).
fn decode_array(doc: &Document, dict: &Dictionary) -> Option<Option<Vec<(f32, f32)>>> {
    let Ok(object) = dict.get(b"Decode") else {
        return Some(None);
    };
    let Some(Object::Array(values)) = deref(doc, object) else {
        return None;
    };
    if values.len() % 2 != 0 {
        return None;
    }

    let mut pairs = Vec::with_capacity(values.len() / 2);
    for pair in values.chunks(2) {
        let low = deref(doc, &pair[0])?.as_float().ok()?;
        let high = deref(doc, &pair[1])?.as_float().ok()?;
        pairs.push((low, high));
    }
    Some(Some(pairs))
}

fn scale_components(
    samples: &[u32; 4],
    max_value: u32,
    count: usize,
    decode: Option<&[(f32, f32)]>,
) -> [u8; 4] {
    let mut result = [0u8; 4];
    for index in 0..count {
        let (low, high) = decode
            .and_then(|pairs| pairs.get(index).copied())
            .unwrap_or((0.0, 1.0));
        let normalized = samples[index] as f32 / max_value as f32;
        let mapped = low + normalized * (high - low);
        result[index] = (mapped.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    result
}

fn rgb_luma([r, g, b, _]: [u8; 4]) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

fn cmyk_luma([c, m, y, k]: [u8; 4]) -> u8 {
    let r = ((255 - c as u32) * (255 - k as u32) / 255) as u8;
    let g = ((255 - m as u32) * (255 - k as u32) / 255) as u8;
    let b = ((255 - y as u32) * (255 - k as u32) / 255) as u8;
    rgb_luma([r, g, b, 0])
}

fn indexed_luma(base: &ColorSpace, sample: &[u8]) -> Option<u8> {
    match base {
        ColorSpace::Gray => Some(sample[0]),
        ColorSpace::Rgb => Some(rgb_luma([sample[0], sample[1], sample[2], 0])),
        ColorSpace::Cmyk => Some(cmyk_luma([sample[0], sample[1], sample[2], sample[3]])),
        ColorSpace::Indexed { .. } => None,
    }
}

fn inflate_zlib(data: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut output)
        .ok()?;
    Some(output)
}

fn dict_int(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    dict.get(key)
        .ok()
        .and_then(|object| deref(doc, object))
        .and_then(|object| object.as_i64().ok())
}

fn dict_bool(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<bool> {
    dict.get(key)
        .ok()
        .and_then(|object| deref(doc, object))
        .and_then(|object| object.as_bool().ok())
}

fn is_invisible_text_mode(mode: i32) -> bool {
    mode == 3 || mode == 7
}

fn effective_resources(doc: &Document, page_id: ObjectId) -> Option<&Dictionary> {
    let mut current = Some(page_id);
    for _ in 0..32 {
        let id = current?;
        let Ok(dict) = doc.get_dictionary(id) else {
            return None;
        };
        if let Ok(resources) = dict.get(b"Resources") {
            return deref_dict(doc, resources);
        }
        current = dict
            .get(b"Parent")
            .ok()
            .and_then(|parent| parent.as_reference().ok());
    }
    None
}

fn deref<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    let mut current = object;
    for _ in 0..32 {
        match current {
            Object::Reference(id) => current = doc.get_object(*id).ok()?,
            _ => return Some(current),
        }
    }
    None
}

fn deref_dict<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    deref(doc, object)?.as_dict().ok()
}

fn is_white(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(byte: u8) -> bool {
    !is_white(byte) && !is_delimiter(byte)
}

fn skip_literal_string(data: &[u8], position: &mut usize) {
    *position += 1;
    let mut depth = 1usize;
    while *position < data.len() {
        match data[*position] {
            b'\\' => *position += 2,
            b'(' => {
                depth += 1;
                *position += 1;
            }
            b')' => {
                depth -= 1;
                *position += 1;
                if depth == 0 {
                    return;
                }
            }
            _ => *position += 1,
        }
    }
}

fn skip_hex_string(data: &[u8], position: &mut usize) {
    *position += 1;
    while *position < data.len() && data[*position] != b'>' {
        *position += 1;
    }
    if *position < data.len() {
        *position += 1;
    }
}

fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            if let (Some(high), Some(low)) = (hex_value(raw[i + 1]), hex_value(raw[i + 2])) {
                decoded.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        decoded.push(raw[i]);
        i += 1;
    }
    decoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn document_with_pages(contents: Vec<Vec<u8>>) -> Document {
        document_with_resources(contents, None)
    }

    fn document_with_resources(
        contents: Vec<Vec<u8>>,
        resources: Option<(Vec<u8>, Stream)>,
    ) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut kids: Vec<Object> = Vec::new();

        let resources_object = resources.map(|(name, stream)| {
            let stream_id = doc.add_object(stream);
            let mut xobjects = Dictionary::new();
            xobjects.set(name, stream_id);
            let mut resources = Dictionary::new();
            resources.set("XObject", xobjects);
            resources
        });

        for content in contents {
            let content_id = doc.add_object(Stream::new(Dictionary::new(), content));
            let mut page = dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            };
            if let Some(resources) = &resources_object {
                page.set("Resources", resources.clone());
            }
            let page_id = doc.add_object(page);
            kids.push(page_id.into());
        }

        let count = kids.len() as i64;
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc
    }

    fn flags_for(doc: &Document, page: u32) -> ContentFlags {
        flags_for_with(doc, page, CleanOptions::default())
    }

    fn flags_for_with(doc: &Document, page: u32, options: CleanOptions) -> ContentFlags {
        let id = doc.get_pages()[&page];
        let mut warnings = Vec::new();
        let mut image_cache = HashMap::new();
        page_flags(doc, id, page, options, &mut image_cache, &mut warnings)
    }

    fn gray_image(width: i64, height: i64, value: u8) -> Stream {
        Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => width,
                "Height" => height,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![value; (width * height) as usize],
        )
    }

    fn jpeg_image(width: u32, height: u32, value: u8) -> Stream {
        let image = image::GrayImage::from_pixel(width, height, image::Luma([value]));
        let mut data = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, 90);
        encoder.encode_image(&image).unwrap();
        Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => width,
                "Height" => height,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
                "Decode" => vec![0.into(), 1.into()],
            },
            data,
        )
    }

    fn flate_image(width: i64, height: i64, value: u8) -> Stream {
        use std::io::Write;

        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&vec![value; (width * height) as usize])
            .unwrap();
        let data = encoder.finish().unwrap();
        Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => width,
                "Height" => height,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
                "Filter" => "FlateDecode",
            },
            data,
        )
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pdf-cleaner-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn page_without_content_is_empty() {
        let doc = document_with_pages(vec![Vec::new()]);
        assert!(!flags_for(&doc, 1).text);
        assert!(!flags_for(&doc, 1).image);
        assert!(!flags_for(&doc, 1).drawing);
    }

    #[test]
    fn text_page_is_not_empty() {
        let doc = document_with_pages(vec![b"BT /F1 12 Tf 72 720 Td (Hello world) Tj ET".to_vec()]);
        assert!(flags_for(&doc, 1).text);
    }

    #[test]
    fn invisible_text_is_ignored() {
        let doc = document_with_pages(vec![
            b"BT 3 Tr /F1 12 Tf 72 720 Td (scanned ocr layer) Tj ET".to_vec(),
        ]);
        let flags = flags_for(&doc, 1);
        assert!(!flags.text);
        assert!(!flags.image);
        assert!(!flags.drawing);
    }

    #[test]
    fn image_xobject_is_content() {
        let image = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1,
                "Height" => 1,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0u8],
        );
        let doc = document_with_pages_with_image(b"q 10 0 0 10 0 0 cm /Im1 Do Q".to_vec(), image);
        let flags = flags_for(&doc, 1);
        assert!(flags.image);
    }

    fn document_with_pages_with_image(content: Vec<u8>, image: Stream) -> Document {
        document_with_resources(vec![content], Some((b"Im1".to_vec(), image)))
    }

    #[test]
    fn unused_image_resource_does_not_keep_page() {
        let image = gray_image(1, 1, 0);
        let doc = document_with_pages_with_image(Vec::new(), image);
        let flags = flags_for(&doc, 1);
        assert!(!flags.text);
        assert!(!flags.image);
        assert!(!flags.drawing);
    }

    fn gray_pixels_image(width: i64, height: i64, pixels: Vec<u8>) -> Stream {
        Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => width,
                "Height" => height,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            pixels,
        )
    }

    fn image_with_band(band_x: i64, band_y: i64, band_width: i64, band_height: i64) -> Stream {
        let size = 400i64;
        let mut pixels = vec![255u8; (size * size) as usize];
        for y in band_y..(band_y + band_height) {
            for x in band_x..(band_x + band_width) {
                pixels[(y * size + x) as usize] = 0;
            }
        }
        gray_pixels_image(size, size, pixels)
    }

    #[test]
    fn blank_white_scan_is_empty() {
        let image = gray_image(200, 200, 255);
        let doc = document_with_pages_with_image(b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(), image);
        let flags = flags_for(&doc, 1);
        assert!(!flags.text);
        assert!(!flags.image);
        assert!(!flags.drawing);
    }

    #[test]
    fn edge_strip_is_blank() {
        // Footer-like strip at the bottom edge: typical bleed-through.
        let image = image_with_band(100, 370, 200, 3);
        let doc = document_with_pages_with_image(b"q 400 0 0 400 0 0 cm /Im1 Do Q".to_vec(), image);
        assert!(!flags_for(&doc, 1).image);
    }

    #[test]
    fn vertical_edge_strip_is_blank() {
        let image = image_with_band(360, 60, 2, 300);
        let doc = document_with_pages_with_image(b"q 400 0 0 400 0 0 cm /Im1 Do Q".to_vec(), image);
        assert!(!flags_for(&doc, 1).image);
    }

    #[test]
    fn centered_strip_is_content() {
        let image = image_with_band(100, 198, 200, 3);
        let doc = document_with_pages_with_image(b"q 400 0 0 400 0 0 cm /Im1 Do Q".to_vec(), image);
        assert!(flags_for(&doc, 1).image);
    }

    #[test]
    fn dark_scan_is_content() {
        let image = gray_image(200, 200, 0);
        let doc = document_with_pages_with_image(b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(), image);
        let flags = flags_for(&doc, 1);
        assert!(flags.image);
    }

    #[test]
    fn decode_array_is_applied() {
        let mut image = gray_image(200, 200, 0);
        image.dict.set("Decode", vec![1.into(), 0.into()]);
        let doc = document_with_pages_with_image(b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(), image);
        assert!(!flags_for(&doc, 1).image);
    }

    #[test]
    fn blank_flate_scan_is_empty() {
        let image = flate_image(200, 200, 255);
        let doc = document_with_pages_with_image(b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(), image);
        assert!(!flags_for(&doc, 1).image);
    }

    #[test]
    fn blank_jpeg_scan_is_empty() {
        let image = jpeg_image(320, 240, 255);
        let doc = document_with_pages_with_image(b"q 320 0 0 240 0 0 cm /Im1 Do Q".to_vec(), image);
        let flags = flags_for(&doc, 1);
        assert!(!flags.image);
    }

    #[test]
    fn jpeg_with_black_mark_is_content() {
        let image = jpeg_image(320, 240, 0);
        let doc = document_with_pages_with_image(b"q 320 0 0 240 0 0 cm /Im1 Do Q".to_vec(), image);
        let flags = flags_for(&doc, 1);
        assert!(flags.image);
    }

    #[test]
    fn scan_analysis_can_be_disabled() {
        let image = gray_image(200, 200, 255);
        let doc = document_with_pages_with_image(b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(), image);
        let options = CleanOptions {
            analyze_scanned_pages: false,
            ..CleanOptions::default()
        };
        assert!(flags_for_with(&doc, 1, options).image);
    }

    #[test]
    fn blank_scan_pages_are_removed_end_to_end() {
        let dir = temp_dir("scans");
        let source = dir.join("scan.pdf");
        let image = gray_image(200, 200, 255);
        let mut doc = document_with_resources(
            vec![
                b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(),
                b"BT /F1 12 Tf 72 720 Td (keep me) Tj ET".to_vec(),
                b"q 200 0 0 200 0 0 cm /Im1 Do Q".to_vec(),
            ],
            Some((b"Im1".to_vec(), image)),
        );
        doc.save(&source).unwrap();

        let report = clean_pdf(&source, CleanOptions::default());
        assert!(report.error.is_none(), "{:?}", report.error);
        assert_eq!(report.removed_pages, vec![1, 3]);
        let cleaned = Document::load(dir.join("scan-cleaned.pdf")).unwrap();
        assert_eq!(cleaned.get_pages().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn vector_only_page_is_not_empty_by_default() {
        let doc = document_with_pages(vec![b"0 0 100 100 re f".to_vec()]);
        let flags = flags_for(&doc, 1);
        assert!(flags.drawing);
        assert!(!flags.text);
        assert!(!flags.image);
    }

    #[test]
    fn form_xobject_text_is_content() {
        let form = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            },
            b"BT /F1 12 Tf 5 5 Td (inside form) Tj ET".to_vec(),
        );
        let doc = document_with_pages_with_image(b"/Fm1 Do".to_vec(), form);
        let flags = flags_for(&doc, 1);
        assert!(flags.text);
    }

    #[test]
    fn recursive_form_does_not_hang() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let form_id = doc.new_object_id();

        let form = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                "Resources" => dictionary! {
                    "XObject" => dictionary! {
                        "Self" => form_id,
                    },
                },
            },
            b"/Self Do".to_vec(),
        );
        doc.objects.insert(form_id, Object::Stream(form));

        let resources = dictionary! {
            "XObject" => dictionary! {
                "Self" => form_id,
            },
        };
        let content_id = doc.add_object(Stream::new(Dictionary::new(), b"/Self Do".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);

        let flags = flags_for(&doc, 1);
        assert!(!flags.text);
        assert!(!flags.image);
        assert!(!flags.drawing);
    }

    #[test]
    fn clean_pdf_removes_blank_pages_and_names_output() {
        let dir = temp_dir("clean");
        let source = dir.join("report.pdf");
        let mut doc = document_with_pages(vec![
            Vec::new(),
            b"BT /F1 12 Tf 72 720 Td (page two) Tj ET".to_vec(),
            Vec::new(),
        ]);
        doc.save(&source).unwrap();

        let report = clean_pdf(&source, CleanOptions::default());
        assert!(report.error.is_none(), "{:?}", report.error);
        assert_eq!(report.total_pages, 3);
        assert_eq!(report.removed_pages, vec![1, 3]);
        assert_eq!(
            report.output.as_deref(),
            Some(dir.join("report-cleaned.pdf").as_path())
        );

        let cleaned = Document::load(dir.join("report-cleaned.pdf")).unwrap();
        assert_eq!(cleaned.get_pages().len(), 1);
        assert!(dir.join("report-cleaned.pdf").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn all_blank_pages_are_refused() {
        let dir = temp_dir("allblank");
        let source = dir.join("blank.pdf");
        let mut doc = document_with_pages(vec![Vec::new(), Vec::new()]);
        doc.save(&source).unwrap();

        let report = clean_pdf(&source, CleanOptions::default());
        assert!(report.error.is_some());
        assert!(!dir.join("blank-cleaned.pdf").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_graphics_option_removes_vector_only_pages() {
        let dir = temp_dir("graphics");
        let source = dir.join("drawing.pdf");
        let mut doc = document_with_pages(vec![
            b"0 0 100 100 re f".to_vec(),
            b"BT /F1 12 Tf 72 720 Td (keep me) Tj ET".to_vec(),
        ]);
        doc.save(&source).unwrap();

        let report = clean_pdf(
            &source,
            CleanOptions {
                remove_graphics_only_pages: true,
                ..CleanOptions::default()
            },
        );
        assert!(report.error.is_none());
        assert_eq!(report.removed_pages, vec![1]);
        let cleaned = Document::load(dir.join("drawing-cleaned.pdf")).unwrap();
        assert_eq!(cleaned.get_pages().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn name_hex_escapes_are_decoded() {
        assert_eq!(decode_name(b"Im#31"), b"Im1");
        assert_eq!(decode_name(b"plain"), b"plain");
    }
}
