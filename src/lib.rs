#[cfg(feature = "capi")]
mod capi;
pub mod docx;
pub mod error;
pub mod field;
pub mod i18n;
pub mod model;
pub mod render;

pub use error::Error;
pub use render::{RenderOptions, DEFAULT_IMAGE_DPI, MIN_IMAGE_DPI};

/// Convert raw DOCX bytes into PDF bytes using default [`RenderOptions`].
pub fn convert(docx_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    convert_with_options(docx_bytes, &RenderOptions::default())
}

/// Convert raw DOCX bytes into PDF bytes with caller-supplied [`RenderOptions`]
/// (e.g. a non-default embedded-image DPI).
pub fn convert_with_options(docx_bytes: &[u8], options: &RenderOptions) -> Result<Vec<u8>, Error> {
    use std::time::Instant;

    let t0 = Instant::now();
    let document = crate::docx::parse(docx_bytes)?;
    log::debug!("Parse:  {:?}", t0.elapsed());

    let t1 = Instant::now();
    let pdf_bytes = crate::render::render(document, options)?;
    log::debug!("Render: {:?}", t1.elapsed());

    log::debug!("Total:  {:?}", t0.elapsed());
    Ok(pdf_bytes)
}

/// [`convert_with_options`] writing the PDF to `out` as it is painted, so it
/// is never held whole. The bytes are the same.
pub fn convert_to_writer(
    docx_bytes: &[u8],
    options: &RenderOptions,
    out: &mut impl std::io::Write,
) -> Result<(), Error> {
    let document = crate::docx::parse(docx_bytes)?;
    crate::render::render_to(document, options, out)?;
    Ok(())
}

/// Convert an opened package to PDF a section at a time, writing to `out`:
/// memory follows the largest section instead of the document (see
/// [`render::stream`]). Lays the document out twice, so it is slower than
/// [`convert_to_writer`] and meant for documents too large for it. The bytes
/// are the same.
pub fn convert_streamed_to(
    package: docx::zip::PackageContents,
    options: &RenderOptions,
    out: &mut impl std::io::Write,
) -> Result<(), Error> {
    let frame = crate::docx::parse::parse_frame(package)?;
    crate::render::stream::render_to(frame.document, &frame.body, options, out)
}

// --- Python bindings (enabled with `python` feature) ---

#[cfg(feature = "python")]
mod python {
    use pyo3::exceptions::PyRuntimeError;
    use pyo3::prelude::*;

    /// Convert DOCX bytes to PDF bytes.
    ///
    /// `image_dpi` sets the target resolution (pixels per inch) embedded raster
    /// images are downsampled to; defaults to 220. Releases the GIL for the
    /// duration of the conversion, so callers can run this on a background
    /// thread rather than needing a subprocess.
    #[pyfunction]
    #[pyo3(signature = (docx_bytes, image_dpi = crate::DEFAULT_IMAGE_DPI))]
    fn convert(py: Python<'_>, docx_bytes: &[u8], image_dpi: f32) -> PyResult<Vec<u8>> {
        // Copied out while the GIL is held: `docx_bytes` zero-copy-borrows a
        // Python-owned buffer, and releasing the GIL below lets another thread
        // mutate or free it (e.g. a `bytearray`) out from under a borrow.
        let docx_bytes = docx_bytes.to_vec();
        py.detach(|| {
            let options = crate::RenderOptions::default().with_image_dpi(image_dpi);
            crate::convert_with_options(&docx_bytes, &options)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))
        })
    }

    /// Convert a DOCX file to a PDF file.
    ///
    /// `image_dpi` sets the target resolution (pixels per inch) embedded raster
    /// images are downsampled to; defaults to 220. Releases the GIL for the
    /// duration of the read, conversion and write, so callers can run this on
    /// a background thread rather than needing a subprocess.
    #[pyfunction]
    #[pyo3(signature = (input, output, image_dpi = crate::DEFAULT_IMAGE_DPI))]
    fn convert_file(py: Python<'_>, input: &str, output: &str, image_dpi: f32) -> PyResult<()> {
        // Copied out while the GIL is held, same reasoning as in `convert`.
        let input = input.to_owned();
        let output = output.to_owned();
        py.detach(|| {
            let docx_bytes = std::fs::read(&input)
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to read {input}: {e}")))?;
            let options = crate::RenderOptions::default().with_image_dpi(image_dpi);
            let pdf_bytes = crate::convert_with_options(&docx_bytes, &options)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            std::fs::write(&output, &pdf_bytes)
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to write {output}: {e}")))
        })
    }

    /// A fast DOCX-to-PDF converter powered by Skia.
    #[pymodule]
    fn dxpdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(convert, m)?)?;
        m.add_function(wrap_pyfunction!(convert_file, m)?)?;
        Ok(())
    }
}
