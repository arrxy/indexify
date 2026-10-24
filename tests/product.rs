use indexify::{
    collection::{self, Collection},
    extract,
};
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use search_engine::retrieval::QueryMode;
use std::{fs, io::Write, path::Path};

fn pdf_fixture(path: &Path) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let mut pages = Vec::new();
    for text in [
        "A guide to indexing documents.",
        "Zircon retrieval uses compressed inverted indexes.",
    ] {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![50.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let contents = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => contents, "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()] });
        pages.push(page.into());
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => pages, "Count" => 2 }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.compress();
    doc.save(path).unwrap();
}

fn docx_fixture(path: &Path) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    zip.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8"?>
    <w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Release runbook</w:t></w:r></w:p>
    <w:p><w:r><w:t>Zircon deployment uses blue </w:t></w:r><w:r><w:t>green &amp; canary releases.</w:t></w:r></w:p>
    <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Rollback checklist</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
    </w:body></w:document>"#).unwrap();
    zip.finish().unwrap();
}

#[test]
fn imports_all_three_formats_with_locations_and_persistent_previews() {
    let temp = tempfile::tempdir().unwrap();
    let documents = temp.path().join("documents");
    fs::create_dir_all(documents.join("nested")).unwrap();
    fs::write(
        documents.join("notes.MD"),
        "# Café search\n\nZircon search handles naïve Unicode notes.\n",
    )
    .unwrap();
    pdf_fixture(&documents.join("paper.PDF"));
    docx_fixture(&documents.join("nested/runbook.docx"));
    fs::write(documents.join("broken.docx"), "not a zip file").unwrap();
    fs::write(documents.join("unsupported.txt"), "must not be indexed").unwrap();
    let home = temp.path().join("library");
    let built = collection::build(&home, "research", &documents, false, &mut |_| Ok(())).unwrap();
    assert_eq!(built.files, 3);
    assert_eq!(built.skipped, 1);
    assert_eq!(built.issues.len(), 1);
    let mut collection = Collection::open(&home, "research").unwrap();
    let found = collection.search("zircon", QueryMode::Disjunctive).unwrap();
    assert_eq!(found.hits.len(), 3);
    let pdf = found
        .hits
        .iter()
        .find(|h| h.passage.format == "PDF")
        .unwrap();
    assert_eq!(pdf.passage.page, Some(2));
    let docx = found
        .hits
        .iter()
        .find(|h| h.passage.format == "DOCX")
        .unwrap();
    assert_eq!(docx.passage.location, "paragraph 2");
    assert_eq!(docx.passage.heading, "Release runbook");
    assert!(docx.text.contains("blue green & canary"));
    let markdown = found
        .hits
        .iter()
        .find(|h| h.passage.format == "MD")
        .unwrap();
    assert_eq!(markdown.passage.line, Some(3));
    assert_eq!(
        collection
            .search("café naïve", QueryMode::Disjunctive)
            .unwrap()
            .hits
            .len(),
        2
    );
    assert!(
        collection::refresh(&home, "research", &mut |_| Ok(())).is_err(),
        "An active reader prevents replacement"
    );
    drop(collection);
    fs::remove_dir_all(&documents).unwrap();
    let mut reopened = Collection::open(&home, "research").unwrap();
    assert_eq!(
        reopened
            .search("zircon", QueryMode::Disjunctive)
            .unwrap()
            .hits
            .len(),
        3
    );
}

#[test]
fn refresh_is_atomic_and_a_failed_refresh_preserves_the_old_index() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("library");
    let source = temp.path().join("notes.md");
    fs::write(&source, "Original cobalt passage.").unwrap();
    collection::build(&home, "notes", &source, false, &mut |_| Ok(())).unwrap();
    let pointer = fs::read(home.join("notes/CURRENT")).unwrap();
    fs::write(&source, "").unwrap();
    assert!(collection::refresh(&home, "notes", &mut |_| Ok(())).is_err());
    assert_eq!(fs::read(home.join("notes/CURRENT")).unwrap(), pointer);
    let mut previous = Collection::open(&home, "notes").unwrap();
    assert_eq!(
        previous
            .search("cobalt", QueryMode::Disjunctive)
            .unwrap()
            .hits
            .len(),
        1
    );
    drop(previous);
    fs::write(&source, "Replacement amber passage.").unwrap();
    collection::refresh(&home, "notes", &mut |_| Ok(())).unwrap();
    let mut current = Collection::open(&home, "notes").unwrap();
    assert!(
        current
            .search("cobalt", QueryMode::Disjunctive)
            .unwrap()
            .hits
            .is_empty()
    );
    assert_eq!(
        current
            .search("amber", QueryMode::Conjunctive)
            .unwrap()
            .hits
            .len(),
        1
    );
    drop(current);
    collection::remove(&home, "notes").unwrap();
    assert!(source.exists());
    assert!(collection::list(&home).unwrap().is_empty());
}

#[test]
fn long_passages_are_bounded_and_names_cannot_escape_storage() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("long.md");
    fs::write(
        &source,
        format!("# Heading\n\n{}\u{1b}[31m", "word ".repeat(500)),
    )
    .unwrap();
    let mut passages = Vec::new();
    extract::extract(&source, &mut |draft| {
        passages.push(draft);
        Ok(())
    })
    .unwrap();
    assert!(passages.len() >= 3);
    assert!(
        passages
            .iter()
            .all(|p| p.text.split_whitespace().count() <= 180 && !p.text.contains('\u{1b}'))
    );
    for name in ["../outside", "", "/absolute", "a/b", "."] {
        assert!(collection::validate_name(name).is_err());
    }
    assert!(collection::validate_name("my-library_2").is_ok());
}

#[test]
fn cancellation_leaves_no_published_collection() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("notes.md");
    fs::write(&source, "Search me.").unwrap();
    let home = temp.path().join("library");
    let result = collection::build(&home, "cancelled", &source, false, &mut |_| {
        anyhow::bail!("cancelled")
    });
    assert!(result.is_err());
    assert!(collection::list(&home).unwrap().is_empty());
}
