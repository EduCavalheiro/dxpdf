//! Renderer error types.

/// Errors that can occur during rendering.
///
/// There is exactly one, and it is the only condition the pipeline cannot
/// render its way out of. Emptiness is deliberately *not* one: a document with
/// no content produces a single blank page (`render::layout_document`), which
/// is what Word does — so the `EmptyDocument` variant this type used to carry
/// was not merely unconstructed, it contradicted the behaviour the pipeline
/// had chosen.
#[derive(Debug)]
pub enum RenderError {
    /// The host font system exposes no typeface at all, so there is nothing to
    /// fall back to when a requested family cannot be resolved. Seen on
    /// container images built without any fonts installed, and with a
    /// deliberately empty `FontMgr`.
    NoFontsAvailable,
    /// The PDF could not be written to the caller's writer (`render_to` and
    /// the other entry points that stream to an `io::Write`). Skia drops
    /// write errors on its side, so this is reported once painting is done.
    Write(std::io::Error),
    /// The second pass of a section-at-a-time render (`render::stream`)
    /// painted another number of pages than the first laid out. Both run the
    /// same layout on the same input, so this is a defect, reported rather
    /// than shipped as a PDF whose page numbers are wrong.
    PassesDiverged { laid_out: usize, painted: usize },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::NoFontsAvailable => write!(
                f,
                "no fonts available on this system — install at least one font, \
                 or check that fontconfig is configured"
            ),
            RenderError::Write(e) => write!(f, "cannot write the PDF: {e}"),
            RenderError::PassesDiverged { laid_out, painted } => write!(
                f,
                "section-at-a-time render diverged: {laid_out} pages laid out, {painted} painted"
            ),
        }
    }
}

impl std::error::Error for RenderError {}
