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
