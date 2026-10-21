//! `convert_streamed_to` (a section at a time, two passes) produces the PDF
//! `convert_with_options` does, byte for byte, on every test document.

use std::path::Path;

use dxpdf::docx::zip::PackageContents;
use dxpdf::model::MediaBytes;
use dxpdf::RenderOptions;

const TEST_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test-files");

fn test_files() -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(TEST_DIR)
        .expect("test-files exists")
        .map(|entry| entry.expect("readable entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "docx"))
        .collect();
    files.sort();
    assert!(files.len() > 30, "the corpus is there");
    files
}

fn whole(data: &[u8]) -> Vec<u8> {
    dxpdf::convert_with_options(data, &RenderOptions::default()).expect("renders")
}

fn streamed(package: PackageContents) -> Vec<u8> {
    let mut pdf = Vec::new();
    dxpdf::convert_streamed_to(package, &RenderOptions::default(), &mut pdf)
        .expect("renders streamed");
    pdf
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn streamed_render_is_byte_identical_on_every_test_document() {
    for path in test_files() {
        let data = std::fs::read(&path).unwrap();
        let Ok(expected) = dxpdf::convert_with_options(&data, &RenderOptions::default()) else {
            // A document the whole-document path refuses is not this test's business.
            continue;
        };
        let package = PackageContents::from_bytes(&data).unwrap();
        assert!(
            streamed(package) == expected,
            "{}: streamed PDF differs",
            name(&path)
        );
    }
}

#[test]
fn writing_to_a_writer_is_byte_identical() {
    for path in test_files() {
        let data = std::fs::read(&path).unwrap();
        let Ok(expected) = dxpdf::convert_with_options(&data, &RenderOptions::default()) else {
            continue;
        };
        let mut pdf = Vec::new();
        dxpdf::convert_to_writer(&data, &RenderOptions::default(), &mut pdf).unwrap();
        assert!(
            pdf == expected,
            "{}: convert_to_writer differs",
            name(&path)
        );
    }
}

/// Media handed over as lazy handles render as the same media in the ZIP.
#[test]
fn media_as_handles_render_as_media_in_the_zip() {
    let mut checked = 0;
    for path in test_files() {
        let data = std::fs::read(&path).unwrap();
        let Ok(expected) = dxpdf::convert_with_options(&data, &RenderOptions::default()) else {
            continue;
        };
        let package = PackageContents::from_bytes(&data).unwrap();
        let media: Vec<(String, MediaBytes)> = package
            .parts
            .iter()
            .filter(|(name, _)| name.contains("/media/"))
            .map(|(name, bytes)| {
                let bytes = bytes.clone();
                (name.clone(), MediaBytes::lazy(move || Some(bytes.clone())))
            })
            .collect();
        if media.is_empty() {
            continue;
        }
        checked += 1;
        let document = dxpdf::docx::parse_package(package.with_media(media)).unwrap();
        let mut pdf = Vec::new();
        dxpdf::render::render_to(document, &RenderOptions::default(), &mut pdf).unwrap();
        assert!(
            pdf == expected,
            "{}: lazy media render differs",
            name(&path)
        );
    }
    assert!(checked > 0, "the corpus has documents with pictures");
}

#[test]
fn a_whole_document_render_matches_the_byte_slice_entry_point() {
    // `render_to` over a parsed document, as `convert_with_options` composes it.
    let path = test_files().into_iter().next().unwrap();
    let data = std::fs::read(&path).unwrap();
    let mut pdf = Vec::new();
    dxpdf::render::render_to(
        dxpdf::docx::parse(&data).unwrap(),
        &RenderOptions::default(),
        &mut pdf,
    )
    .unwrap();
    assert!(pdf == whole(&data));
}

/// The body read block by block is the body read whole, model for model.
#[test]
fn the_streamed_body_is_the_parsed_body() {
    use dxpdf::docx::parse::body::BodyItem;
    for path in test_files() {
        let data = std::fs::read(&path).unwrap();
        let Ok(frame) =
            dxpdf::docx::parse::parse_frame(PackageContents::from_bytes(&data).unwrap())
        else {
            continue;
        };
        let whole = frame
            .body
            .body()
            .map(|(blocks, end)| format!("{blocks:?} {end:?}"));
        let mut stream = frame.body.stream_body();
        let mut blocks = Vec::new();
        let streamed = loop {
            match stream.next_item() {
                Ok(Some(BodyItem::Block(block))) => blocks.push(block),
                Ok(Some(BodyItem::End(end))) => break Ok(format!("{blocks:?} {end:?}")),
                Ok(None) => panic!("{}: ended without its final section", name(&path)),
                Err(e) => break Err(e),
            }
        };
        match (whole, streamed) {
            (Ok(a), Ok(b)) => assert!(a == b, "{}: streamed body differs", name(&path)),
            (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{}", name(&path)),
            (a, b) => panic!(
                "{}: whole {:?} / streamed {:?}",
                name(&path),
                a.is_ok(),
                b.is_ok()
            ),
        }
    }
}

/// A body nested deeper than Rust's default thread stack would hold: the
/// reader thread must not overflow where the caller's thread did not. The
/// caller here is a thread with a main thread's stack (8 MiB), which parses
/// it whole; a test thread's 2 MiB would not.
#[test]
fn deeply_nested_tables_stream_as_they_parse() {
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(deeply_nested_tables)
        .unwrap()
        .join()
        .unwrap();
}

fn deeply_nested_tables() {
    use std::io::Write;
    let depth = 400;
    let mut body = String::new();
    for _ in 0..depth {
        body.push_str("<w:tbl><w:tr><w:tc>");
    }
    body.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
    for _ in 0..depth {
        body.push_str("</w:tc></w:tr></w:tbl><w:p/>");
    }
    let document = format!(
        r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:p><w:pPr><w:sectPr/></w:pPr></w:p></w:body></w:document>"#
    );
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for (part, xml) in [
        ("[Content_Types].xml", r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/></Types>"#.to_owned()),
        ("_rels/.rels", r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_owned()),
        ("word/document.xml", document),
    ] {
        zip.start_file(part, opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
    }
    let data = zip.finish().unwrap().into_inner();
    let frame =
        dxpdf::docx::parse::parse_frame(PackageContents::from_bytes(&data).unwrap()).unwrap();
    let whole = frame.body.body().map(|(blocks, _)| blocks.len());
    let mut stream = frame.body.stream_body();
    let mut count = 0;
    let streamed = loop {
        match stream.next_item() {
            Ok(Some(dxpdf::docx::parse::body::BodyItem::Block(_))) => count += 1,
            Ok(_) => break Ok(count),
            Err(e) => break Err(e),
        }
    };
    assert_eq!(whole.ok(), streamed.ok(), "same outcome both ways");
}

/// The section-at-a-time path with media as lazy handles, as
/// docx-template-render calls it, against the whole-document path.
#[test]
fn streamed_render_with_lazy_media_is_byte_identical() {
    for path in test_files() {
        let data = std::fs::read(&path).unwrap();
        let Ok(expected) = dxpdf::convert_with_options(&data, &RenderOptions::default()) else {
            continue;
        };
        let package = PackageContents::from_bytes(&data).unwrap();
        let media: Vec<(String, MediaBytes)> = package
            .parts
            .iter()
            .filter(|(name, _)| name.contains("/media/"))
            .map(|(name, bytes)| {
                let bytes = bytes.clone();
                (name.clone(), MediaBytes::lazy(move || Some(bytes.clone())))
            })
            .collect();
        if media.is_empty() {
            continue;
        }
        assert!(
            streamed(package.with_media(media)) == expected,
            "{}: differs",
            name(&path)
        );
    }
}
