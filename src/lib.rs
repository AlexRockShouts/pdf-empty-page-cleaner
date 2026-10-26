//! Core logic for removing empty pages from PDF files.
//!
//! A page is considered empty when it does not draw any visible text and does
//! not draw any image. Vector graphics are treated as content by default, but
//! can optionally be ignored.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FORM_DEPTH: usize = 16;
const MAX_WARNINGS: usize = 12;

#[derive(Debug, Clone, Copy, Default)]
pub struct CleanOptions {
    /// Also remove pages whose only content is vector graphics (lines, shapes).
    pub remove_graphics_only_pages: bool,
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

    for (number, page_id) in &pages {
        let flags = page_flags(&doc, *page_id, *number, &mut warnings);
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

fn page_flags(
    doc: &Document,
    page_id: ObjectId,
    page_number: u32,
    warnings: &mut Vec<String>,
) -> ContentFlags {
    let scanner = Scanner { doc };
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
}

impl<'a> Scanner<'a> {
    fn scan_stream(
        &self,
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
        &self,
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
            b"Image" => flags.image = true,
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
        &self,
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
        &self,
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
        let id = doc.get_pages()[&page];
        let mut warnings = Vec::new();
        page_flags(doc, id, page, &mut warnings)
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
        let doc = document_with_pages_with_image(Vec::new(), image);
        let flags = flags_for(&doc, 1);
        assert!(!flags.text);
        assert!(!flags.image);
        assert!(!flags.drawing);
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
