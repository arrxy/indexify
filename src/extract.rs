//! Extract one source at a time; emit bounded passages while keeping source locations.
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

pub struct Draft {
    pub text: String,
    pub heading: String,
    pub location: String,
    pub page: Option<u32>,
    pub line: Option<usize>,
}

pub fn format(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "pdf" => Some("PDF"),
        "md" => Some("MD"),
        "docx" => Some("DOCX"),
        _ => None,
    }
}

pub fn extract(path: &Path, emit: &mut dyn FnMut(Draft) -> Result<()>) -> Result<usize> {
    match format(path) {
        Some("MD") => markdown(path, emit),
        Some("PDF") => pdf(path, emit),
        Some("DOCX") => docx(path, emit),
        _ => bail!("Supported formats: PDF, MD, DOCX"),
    }
}

fn emit_chunks(
    text: &str,
    heading: &str,
    location: &str,
    page: Option<u32>,
    line: Option<usize>,
    emit: &mut dyn FnMut(Draft) -> Result<()>,
) -> Result<usize> {
    // Strip terminal control characters, including escapes embedded in documents.
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let words: Vec<_> = clean.split_whitespace().collect();
    if !clean.chars().any(char::is_alphanumeric) {
        return Ok(0);
    }
    let mut start = 0;
    let mut count = 0;
    while start < words.len() {
        let end = (start + 180).min(words.len());
        emit(Draft {
            text: words[start..end].join(" "),
            heading: heading.to_owned(),
            location: location.to_owned(),
            page,
            line,
        })?;
        count += 1;
        if end == words.len() {
            break;
        }
        start = end - 24; // Context overlap for long paragraphs and PDF pages.
    }
    Ok(count)
}

fn markdown(path: &Path, emit: &mut dyn FnMut(Draft) -> Result<()>) -> Result<usize> {
    let reader = BufReader::new(File::open(path)?);
    let mut block = String::new();
    let mut heading = String::new();
    let mut start_line = 1;
    let mut count = 0;
    let mut in_code = false;
    for (index, line) in reader.lines().enumerate() {
        let line = line.context("Markdown must be valid UTF-8")?;
        let trimmed = line.trim();
        let is_heading = !in_code
            && trimmed.starts_with('#')
            && trimmed.trim_start_matches('#').starts_with(' ');
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_heading || trimmed.is_empty() || block.len() >= 6000 {
            count += emit_chunks(
                &block,
                &heading,
                &format!("line {start_line}"),
                None,
                Some(start_line),
                emit,
            )?;
            block.clear();
            start_line = index + 1;
        }
        if is_heading {
            heading = trimmed.trim_start_matches('#').trim().to_owned();
        }
        if is_fence {
            in_code = !in_code;
            continue;
        }
        if !trimmed.is_empty() {
            if block.is_empty() {
                start_line = index + 1;
            }
            block.push_str(&line);
            block.push('\n');
        }
    }
    count += emit_chunks(
        &block,
        &heading,
        &format!("line {start_line}"),
        None,
        Some(start_line),
        emit,
    )?;
    Ok(count)
}

fn pdf(path: &Path, emit: &mut dyn FnMut(Draft) -> Result<()>) -> Result<usize> {
    let document = lopdf::Document::load(path).context("Cannot read PDF")?;
    if document.is_encrypted() {
        bail!("Password-protected PDF; provide an unencrypted copy");
    }
    let mut count = 0;
    let mut unreadable_pages = Vec::new();
    for page in document.get_pages().keys() {
        let text = match document.extract_text(&[*page]) {
            Ok(text) => text,
            Err(error) => {
                unreadable_pages.push(format!("{page}: {error}"));
                continue;
            }
        };
        count += emit_chunks(&text, "", &format!("page {page}"), Some(*page), None, emit)?;
    }
    if !unreadable_pages.is_empty() {
        bail!(
            "PDF text extraction failed on page(s) {}",
            unreadable_pages.join(", ")
        );
    }
    if count == 0 {
        bail!("No selectable text found. Scanned PDFs need OCR before indexing");
    }
    Ok(count)
}

fn docx(path: &Path, emit: &mut dyn FnMut(Draft) -> Result<()>) -> Result<usize> {
    let mut archive =
        zip::ZipArchive::new(File::open(path)?).context("Cannot read DOCX archive")?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")?
        .take(64 * 1024 * 1024 + 1)
        .read_to_string(&mut xml)?;
    if xml.len() > 64 * 1024 * 1024 {
        bail!("DOCX document XML exceeds 64 MiB");
    }
    let document = roxmltree::Document::parse(&xml).context("Malformed Word document XML")?;
    let mut count = 0;
    let mut heading = String::new();
    let mut paragraph = 0;
    for node in document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "p")
    {
        paragraph += 1;
        let mut text = String::new();
        for child in node.descendants().filter(|n| n.is_element()) {
            // Text boxes can contain nested paragraphs; do not index them twice.
            if child
                .ancestors()
                .skip(1)
                .find(|n| n.is_element() && n.tag_name().name() == "p")
                != Some(node)
            {
                continue;
            }
            match child.tag_name().name() {
                "t" => text.push_str(child.text().unwrap_or("")),
                "tab" | "br" | "cr" => text.push(' '),
                _ => {}
            }
        }
        let style = node
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "pStyle")
            .and_then(|n| {
                n.attributes()
                    .find(|a| a.name() == "val")
                    .map(|a| a.value())
            });
        if style.is_some_and(|s| s.to_ascii_lowercase().starts_with("heading") || s == "Title") {
            heading = text.clone();
        }
        count += emit_chunks(
            &text,
            &heading,
            &format!("paragraph {paragraph}"),
            None,
            None,
            emit,
        )?;
    }
    Ok(count)
}
