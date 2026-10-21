//! Rendering a document a section at a time, for documents too large to
//! hold laid out whole.
//!
//! [`render`](super::render) parses the whole body, lays out every page, and
//! only then paints: memory grows with the page count. Here the body is read
//! block by block ([`BodySource::stream_body`]), cut into sections as they
//! end, and each section is laid out, painted and dropped before the next is
//! read. What has to be known before the first page is painted (NUMPAGES,
//! the PDF outline, the glyphs each font subset keeps, the last page each
//! picture is drawn on) comes from a first pass that lays the sections out
//! the same way and keeps only those facts. Memory then follows the largest
//! section, not the document.
//!
//! The PDF is byte for byte the one `render` produces. Both run the same
//! layout steps ([`DocumentLayout`]) in the same order on the same input:
//!
//! - a section is laid out with the sections a shared (continuous) last page
//!   looks ahead into, as `layout_document` gives it the whole list;
//! - headers and footers (phase 2) run against the state phase 2 starts
//!   from in `layout_document`, the one after every section and the
//!   endnotes, captured by the first pass;
//! - layout measures with the fonts as resolved, and paint draws with their
//!   subsets, as there; here from two registries sharing the same faces
//!   ([`fonts::FontRegistry::fork`]).

use std::collections::{HashMap, VecDeque};

use crate::docx::parse::body::{BodyItem, BodyStream};
use crate::docx::parse::BodySource;
use crate::model::Document;
use crate::render::layout::build::{BuildContext, BuildState};
use crate::render::layout::draw_command::{coalesce_abutting_rects, DrawCommand, LayoutedPage};
use crate::render::layout::measurer::TextMeasurer;
use crate::render::layout::page::PageConfig;
use crate::render::painter::{self, ImageCacheKey};
use crate::render::resolve::sections::{ResolvedSection, SectionSplitter};
use crate::render::resolve::{self, HeaderFooterParts};
use crate::render::{fonts, DocumentEndnote, DocumentLayout, RenderOptions, SectionHf};

/// Render a document whose body is still to be read (`body`, from a
/// [`Frame`](crate::docx::parse::Frame)) to `out`, a section at a time (see
/// the module docs).
pub fn render_to(
    document: Document,
    body: &BodySource,
    options: &RenderOptions,
    out: &mut impl std::io::Write,
) -> Result<(), crate::Error> {
    let (resolved, parts) = resolve::resolve_frame(document);
    let registry = fonts::FontRegistry::build(skia_safe::FontMgr::new(), &resolved.embedded_fonts)?;
    let image_dpi = options.image_dpi();

    // Pass 1: lay out, keep the facts, drop the pages.
    let facts = {
        let measurer = TextMeasurer::new(&registry);
        let ctx = BuildContext {
            measurer: &measurer,
            resolved: &resolved,
        };
        first_pass(&ctx, &parts, body.stream_body(), &registry, image_dpi)?
    };

    #[cfg(feature = "subset-fonts")]
    let paint_registry = {
        let mut paint = registry.fork();
        let report = crate::render::subset::apply(facts.usage, &mut paint);
        log::info!("font subset: {report}");
        paint
    };
    #[cfg(not(feature = "subset-fonts"))]
    let paint_registry = registry.fork();
    let structure_tree = painter::build_outline_tree(&facts.outline);

    // Pass 2: lay out again, the same way, and paint each page once it is final.
    let measurer = TextMeasurer::new(&registry);
    let ctx = BuildContext {
        measurer: &measurer,
        resolved: &resolved,
    };
    let mut pages = SecondPass {
        layout: DocumentLayout::at(&ctx, facts.clock),
        sections: Sections::new(body.stream_body(), &parts),
        hf_state: facts.hf_state,
        total_pages: facts.total_pages,
        endnotes: Some(facts.endnotes),
        ready: VecDeque::new(),
        next_page: 0,
        error: None,
    };
    let expected = facts.total_pages + facts.extra_pages;
    let painted = painter::paint_pages(
        structure_tree,
        facts.last_uses,
        std::iter::from_fn(|| pages.next_page()),
        &paint_registry,
        options,
        out,
    )
    .map_err(crate::render::error::RenderError::Write)?;
    // On an error here `out` holds a closed PDF of fewer pages; the caller
    // gets the error and discards it.
    if let Some(e) = pages.error {
        return Err(e);
    }
    if painted != expected {
        return Err(crate::render::error::RenderError::PassesDiverged {
            laid_out: expected,
            painted,
        }
        .into());
    }
    Ok(())
}

/// The sections of a streamed body, in order, read ahead as far as laying
/// out the first one needs.
struct Sections<'p> {
    body: BodyStream,
    /// `None` once the body's end was read.
    splitter: Option<SectionSplitter>,
    parts: &'p HeaderFooterParts,
    window: VecDeque<ResolvedSection>,
}

impl<'p> Sections<'p> {
    fn new(body: BodyStream, parts: &'p HeaderFooterParts) -> Self {
        Sections {
            body,
            splitter: Some(SectionSplitter::default()),
            parts,
            window: VecDeque::new(),
        }
    }

    /// Reads the next section into the window; `false` once there is none.
    fn read_one(&mut self) -> Result<bool, crate::Error> {
        loop {
            if self.splitter.is_none() {
                return Ok(false);
            }
            match self.body.next_item()? {
                Some(BodyItem::Block(block)) => {
                    let splitter = self.splitter.as_mut().expect("checked above");
                    if let Some(section) =
                        splitter.push(block, &self.parts.headers, &self.parts.footers)
                    {
                        self.window.push_back(section);
                        return Ok(true);
                    }
                }
                Some(BodyItem::End(final_section)) => {
                    let splitter = self.splitter.take().expect("checked above");
                    self.window.push_back(splitter.finish(
                        *final_section,
                        &self.parts.headers,
                        &self.parts.footers,
                    ));
                    return Ok(true);
                }
                None => {
                    self.splitter = None;
                    return Ok(false);
                }
            }
        }
    }

    /// Fills the window so that laying out its first section sees what
    /// `layout_document` would show it: the section after it, and after each
    /// continuous one (a shared last page looks along the chain). `false`
    /// when there is no section left.
    fn fill(&mut self) -> Result<bool, crate::Error> {
        if self.window.is_empty() && !self.read_one()? {
            return Ok(false);
        }
        while self.window.len() < 2 || self.window.back().is_some_and(is_continuous) {
            if !self.read_one()? {
                break;
            }
        }
        Ok(true)
    }

    /// Lays out the first section of the window and removes it from there.
    fn lay_out_next(
        &mut self,
        layout: &mut DocumentLayout,
    ) -> (Vec<LayoutedPage>, SectionHf, ResolvedSection) {
        let (pages, hf) = layout.section(self.window.make_contiguous(), 0);
        let section = self.window.pop_front().expect("`fill` returned true");
        (pages, hf, section)
    }
}

fn is_continuous(section: &ResolvedSection) -> bool {
    section.properties.section_type.get() == Some(&crate::model::SectionType::Continuous)
}

/// What the first pass learns, for the second.
struct Facts {
    clock: (crate::field::context::Date, crate::field::context::Time),
    /// NUMPAGES: the pages of the sections, the endnote page aside.
    total_pages: usize,
    /// Pages painted after the sections: the endnote page, or the blank
    /// page of a document with no other.
    extra_pages: usize,
    #[cfg(feature = "subset-fonts")]
    usage: crate::render::subset::collect::CodepointUsage,
    /// The outline marks of every page, in order (pages carrying only those).
    outline: Vec<LayoutedPage>,
    /// The last page each cached picture is drawn on.
    last_uses: HashMap<ImageCacheKey, usize>,
    /// The state phase 2 starts from.
    hf_state: BuildState,
    endnotes: Vec<DocumentEndnote>,
}

impl Facts {
    /// Records what `pages`, the ones from index `first` on, contribute.
    fn note(
        &mut self,
        pages: &[LayoutedPage],
        first: usize,
        _registry: &fonts::FontRegistry,
        image_dpi: f32,
    ) {
        #[cfg(feature = "subset-fonts")]
        self.usage
            .merge(crate::render::subset::collect::collect(pages, _registry));
        for (offset, page) in pages.iter().enumerate() {
            let index = first + offset;
            for key in painter::image_keys(page, image_dpi) {
                let last = self.last_uses.entry(key).or_insert(index);
                *last = (*last).max(index);
            }
        }
    }

    fn note_outline(&mut self, pages: &[LayoutedPage]) {
        self.outline.extend(pages.iter().map(|page| {
            LayoutedPage {
                commands: page
                    .commands
                    .iter()
                    .filter(|cmd| matches!(cmd, DrawCommand::Outline(_)))
                    .cloned()
                    .collect(),
                page_size: page.page_size,
            }
        }));
    }
}

/// Pass 1: what `layout_document` would produce, a section at a time, kept
/// only as [`Facts`].
fn first_pass(
    ctx: &BuildContext,
    parts: &HeaderFooterParts,
    body: BodyStream,
    registry: &fonts::FontRegistry,
    image_dpi: f32,
) -> Result<Facts, crate::Error> {
    let mut layout = DocumentLayout::new(ctx);
    let mut sections = Sections::new(body, parts);
    let mut facts = Facts {
        clock: layout.clock(),
        total_pages: 0,
        extra_pages: 0,
        #[cfg(feature = "subset-fonts")]
        usage: Default::default(),
        outline: Vec::new(),
        last_uses: HashMap::new(),
        hf_state: BuildState::default(),
        endnotes: Vec::new(),
    };

    // Phase 1, section by section. A section's headers and footers are kept
    // (not its blocks) for phase 2 below.
    let mut laid_out: Vec<(ResolvedSection, SectionHf, usize)> = Vec::new();
    let mut page_count = 0;
    while sections.fill()? {
        let (pages, hf, section) = sections.lay_out_next(&mut layout);
        facts.note(&pages, page_count, registry, image_dpi);
        facts.note_outline(&pages);
        let headers_only = ResolvedSection {
            blocks: Vec::new(),
            ..section
        };
        laid_out.push((headers_only, hf, page_count));
        page_count += pages.len();
    }

    facts.endnotes = layout.endnotes();
    facts.hf_state = layout.state.clone();
    facts.total_pages = page_count;

    // Phase 2 draws headers and footers over the pages without reading them,
    // so blank pages of the same size show what it adds (text for the font
    // subsets, pictures). It runs on a copy: pass 2 starts from the original.
    let mut state = facts.hf_state.clone();
    for (section, hf, first) in &laid_out {
        let mut pages: Vec<LayoutedPage> = (0..hf.pages)
            .map(|_| LayoutedPage::new(hf.config.page_size))
            .collect();
        layout.headers_footers_with(&mut state, &mut pages, section, hf, *first, page_count);
        facts.note(&pages, *first, registry, image_dpi);
    }

    if let Some(page) = layout.endnote_page(&facts.endnotes) {
        let pages = [page];
        facts.note(&pages, page_count, registry, image_dpi);
        facts.note_outline(&pages);
        facts.extra_pages = 1;
    } else if page_count == 0 {
        facts.extra_pages = 1;
    }
    Ok(facts)
}

/// Pass 2: the pages `layout_document` returns, one at a time, each final
/// (headers, footers, coalesced fills) when it is handed out.
struct SecondPass<'a, 'p> {
    layout: DocumentLayout<'a>,
    sections: Sections<'p>,
    hf_state: BuildState,
    total_pages: usize,
    /// Taken for the endnote page, after the last section.
    endnotes: Option<Vec<DocumentEndnote>>,
    ready: VecDeque<LayoutedPage>,
    next_page: usize,
    error: Option<crate::Error>,
}

impl SecondPass<'_, '_> {
    fn next_page(&mut self) -> Option<LayoutedPage> {
        loop {
            if let Some(page) = self.ready.pop_front() {
                return Some(page);
            }
            if self.error.is_some() {
                return None;
            }
            match self.sections.fill() {
                Err(e) => {
                    self.error = Some(e);
                    return None;
                }
                Ok(true) => {
                    let (mut pages, hf, section) = self.sections.lay_out_next(&mut self.layout);
                    self.layout.headers_footers_with(
                        &mut self.hf_state,
                        &mut pages,
                        &section,
                        &hf,
                        self.next_page,
                        self.total_pages,
                    );
                    for page in &mut pages {
                        coalesce_abutting_rects(&mut page.commands);
                    }
                    self.next_page += pages.len();
                    self.ready.extend(pages);
                }
                Ok(false) => {
                    // After the last section: the endnote page, or the blank
                    // page an empty document still gets.
                    let endnotes = self.endnotes.take()?;
                    let mut page = self.layout.endnote_page(&endnotes);
                    if page.is_none() && self.next_page == 0 {
                        page = Some(LayoutedPage::new(PageConfig::default().page_size));
                    }
                    let mut page = page?;
                    coalesce_abutting_rects(&mut page.commands);
                    self.next_page += 1;
                    return Some(page);
                }
            }
        }
    }
}
