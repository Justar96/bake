//! Bounded SVG for Bake's `visualize` tool: checks, streaming repair, and
//! rasterizing, on [resvg].
//!
//! The model writes an SVG document as a tool argument, so the source is
//! untrusted. [`parse`] refuses what could read outside the document or
//! grow without bound before resvg sees it:
//!
//! - sources over [`MAX_SOURCE_BYTES`];
//! - entity declarations (`<!ENTITY`), which could expand exponentially;
//! - a root element other than `<svg>`;
//! - more than [`MAX_NODES`] elements once every `<use>` is replaced by a
//!   copy of what it links to and every marker is drawn at each vertex of
//!   the shapes that may take one: nesting multiplies both, so they are
//!   counted from the source before resvg builds anything;
//! - trees of more than [`MAX_NODES`] nodes once parsed.
//!
//! Linked resources are never loaded: the resolvers for `<image>` links,
//! data URLs included, answer nothing, so a link to a file or a URL draws
//! nothing. Fonts come from [`Fonts`]: [`Fonts::system`] reads the system's
//! fonts once; [`Fonts::none`] has none, so text draws nothing, which keeps
//! tests independent of the host. Parsing and drawing run under
//! `catch_unwind`, so a panic in resvg on hostile input is an error here.
//!
//! While the source streams, [`repairs`] gives documents that parse: the
//! source up to one of its last complete tags, with the elements still open
//! closed. The caller draws the first that parses, so a drawing grows as
//! the source arrives and never shows a half-written tag.
//!
//! [`Svg::render`] draws at a pixel size the caller chooses with [`fit`],
//! which keeps the drawing's proportions for pixels of any shape, such as
//! the half cells a terminal draws with. Its [`Raster`] holds straight
//! (not premultiplied) RGBA.
//!
//! [`Svg::report`] measures what a drawing's author cannot see: the texts
//! as laid out, with their boxes and sizes on the canvas, and the shapes
//! that fall wholly outside it.

use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use resvg::tiny_skia;
use resvg::usvg;
use resvg::usvg::fontdb;
use resvg::usvg::roxmltree;

/// Most bytes a source takes.
pub const MAX_SOURCE_BYTES: usize = 128 * 1024;
/// Most elements a source expands to, copies made by `<use>` and markers
/// included, and most nodes a parsed tree holds.
pub const MAX_NODES: usize = 20_000;
/// Most cells an image is drawn in, across and down: the usual size.
pub const CELLS: (u32, u32) = (96, 20);
/// Most cells an image asked to be large is drawn in.
pub const LARGE_CELLS: (u32, u32) = (144, 32);
/// The pixels a cell is taken to have, across and down, where the terminal
/// shows images and its own cell size is not known.
pub const CELL_PIXELS: (u32, u32) = (10, 20);
/// Most texts [`Svg::report`] measures.
pub const MAX_REPORTED_TEXTS: usize = 400;
/// Characters of a text [`TextBox::label`] keeps.
const LABEL_CHARS: usize = 32;
/// Most pixels [`Svg::render`] draws; a larger size is scaled down.
pub const MAX_PIXELS: u64 = 4_000_000;
/// Deepest element nesting the parser accepts.
const MAX_DEPTH: u32 = 128;
/// Complete tags [`repairs`] goes back over, the latest first.
const REPAIRS: usize = 4;

/// Why a source does not draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Over [`MAX_SOURCE_BYTES`].
    TooLarge { bytes: usize },
    /// It declares entities.
    Entities,
    /// Its root element is not `<svg>`, or it has none.
    NotSvg,
    /// It is not well-formed XML; `line` and `column` count from 1.
    Malformed {
        line: u32,
        column: u32,
        message: String,
    },
    /// Its width, height, or viewBox gives no positive size.
    NoSize,
    /// Over [`MAX_NODES`] elements with `<use>` copies and markers
    /// counted, or nodes once parsed.
    TooManyNodes,
    /// resvg failed on it.
    Failed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes } => write!(
                f,
                "the source is {bytes} bytes; the limit is {MAX_SOURCE_BYTES}"
            ),
            Self::Entities => f.write_str("entity declarations (<!ENTITY) are not supported"),
            Self::NotSvg => f.write_str("the root element must be <svg>"),
            Self::Malformed {
                line,
                column,
                message,
            } => write!(f, "line {line}, column {column}: {message}"),
            Self::NoSize => {
                f.write_str("the <svg> needs a viewBox, or a positive width and height")
            }
            Self::TooManyNodes => write!(
                f,
                "the drawing has over {MAX_NODES} elements, counting each <use> as a copy of what it links to and each marker once per vertex"
            ),
            Self::Failed => f.write_str("the SVG renderer failed on this source"),
        }
    }
}

impl std::error::Error for Error {}

/// The fonts text draws with.
#[derive(Clone)]
pub struct Fonts {
    db: Arc<fontdb::Database>,
    family: String,
}

impl fmt::Debug for Fonts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fonts")
            .field("faces", &self.db.len())
            .field("family", &self.family)
            .finish()
    }
}

/// Families tried in order for each generic family, the first installed
/// taken; otherwise the first face's family.
const SANS: [&str; 8] = [
    "DejaVu Sans",
    "Noto Sans",
    "Liberation Sans",
    "Segoe UI",
    "Arial",
    "Helvetica",
    "Ubuntu",
    "Cantarell",
];
const SERIF: [&str; 5] = [
    "DejaVu Serif",
    "Noto Serif",
    "Liberation Serif",
    "Times New Roman",
    "Georgia",
];
const MONO: [&str; 7] = [
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Cascadia Mono",
    "Consolas",
    "Menlo",
    "Courier New",
];

impl Fonts {
    /// No fonts: text draws nothing.
    pub fn none() -> Self {
        Self {
            db: Arc::new(fontdb::Database::new()),
            family: SANS[0].to_owned(),
        }
    }

    /// The system's fonts, with `sans-serif`, `serif`, and `monospace`
    /// mapped to installed families. Reads every font directory the
    /// platform names, so call it once, off the drawing thread.
    pub fn system() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let installed = |wanted: &[&str]| {
            wanted
                .iter()
                .find(|name| {
                    db.faces()
                        .any(|face| face.families.iter().any(|(family, _)| family == *name))
                })
                .map(|name| (*name).to_owned())
                .or_else(|| {
                    db.faces()
                        .next()
                        .and_then(|face| face.families.first())
                        .map(|(family, _)| family.clone())
                })
        };
        let sans = installed(&SANS);
        let serif = installed(&SERIF);
        let mono = installed(&MONO);
        if let Some(sans) = &sans {
            db.set_sans_serif_family(sans.clone());
        }
        if let Some(serif) = serif {
            db.set_serif_family(serif);
        }
        if let Some(mono) = mono {
            db.set_monospace_family(mono);
        }
        Self {
            db: Arc::new(db),
            family: sans.unwrap_or_else(|| SANS[0].to_owned()),
        }
    }

    /// How many font faces there are.
    pub fn faces(&self) -> usize {
        self.db.len()
    }
}

/// What a parsed drawing holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outline {
    /// Its size in user units.
    pub width: f32,
    pub height: f32,
    /// The elements its source holds inside the root, drawn or not.
    pub elements: usize,
    /// Its nodes once parsed: groups, paths, images, and texts that draw.
    pub nodes: usize,
    /// Its text elements.
    pub texts: usize,
}

/// A text as laid out on the canvas, in the canvas's units.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    /// Its characters on one line, cut to a few words.
    pub label: String,
    /// Its smallest font size once its transforms apply.
    pub size: f32,
    /// The box its glyphs fill.
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl TextBox {
    /// Whether none of the text is on a canvas of `width` by `height`.
    pub fn outside(&self, width: f32, height: f32) -> bool {
        self.right <= 0.0 || self.bottom <= 0.0 || self.left >= width || self.top >= height
    }

    /// How far the text passes the canvas's edges: left, top, right, and
    /// bottom, each 0 when it does not.
    pub fn past(&self, width: f32, height: f32) -> [f32; 4] {
        [
            (-self.left).max(0.0),
            (-self.top).max(0.0),
            (self.right - width).max(0.0),
            (self.bottom - height).max(0.0),
        ]
    }

    /// The share of the smaller of the two boxes that the other covers,
    /// from 0 to 1.
    pub fn overlap(&self, other: &Self) -> f32 {
        let area =
            |text: &Self| (text.right - text.left).max(0.0) * (text.bottom - text.top).max(0.0);
        let width = self.right.min(other.right) - self.left.max(other.left);
        let height = self.bottom.min(other.bottom) - self.top.max(other.top);
        let smaller = area(self).min(area(other));
        if width <= 0.0 || height <= 0.0 || smaller <= 0.0 {
            return 0.0;
        }
        (width * height / smaller).clamp(0.0, 1.0)
    }
}

/// What a drawing's author cannot see without looking at it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// The texts that draw, in document order, at most
    /// [`MAX_REPORTED_TEXTS`]. Empty without fonts: text then draws
    /// nothing.
    pub texts: Vec<TextBox>,
    /// The shapes that draw, clipped or masked ones left out.
    pub shapes: usize,
    /// Those of them wholly outside the canvas.
    pub hidden_shapes: usize,
}

/// A parsed drawing.
pub struct Svg {
    tree: usvg::Tree,
    outline: Outline,
}

impl fmt::Debug for Svg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Svg")
            .field("outline", &self.outline)
            .finish()
    }
}

/// Pixels in straight RGBA, row by row.
#[derive(Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl fmt::Debug for Raster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Raster")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl Raster {
    /// The share of the pixels less than half covered, from 0 to 1: those
    /// a terminal leaves to its own background.
    pub fn clear_share(&self) -> f32 {
        let (pixels, _) = self.rgba.as_chunks::<4>();
        if pixels.is_empty() {
            return 1.0;
        }
        let clear = pixels.iter().filter(|pixel| pixel[3] < 128).count();
        clear as f32 / pixels.len() as f32
    }

    /// The pixel at `x`, `y`; transparent outside the raster.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let at = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ]
    }
}

/// Parses `source`, refusing what the crate documentation lists.
pub fn parse(source: &str, fonts: &Fonts) -> Result<Svg, Error> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(Error::TooLarge {
            bytes: source.len(),
        });
    }
    if source.contains("<!ENTITY") {
        return Err(Error::Entities);
    }
    match first_element(source) {
        Some(name) if local(name) == "svg" => {}
        Some(_) => return Err(Error::NotSvg),
        // No element yet: let the parser say where it is malformed.
        None => {}
    }
    let options = usvg::Options {
        resources_dir: None,
        font_family: fonts.family.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: Arc::clone(&fonts.db),
        max_depth: MAX_DEPTH,
        ..usvg::Options::default()
    };
    // As `usvg::Tree::from_str` reads it. Entities are refused above, so
    // a DTD declares nothing that expands.
    let xml = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        let document = roxmltree::Document::parse_with_options(source, xml)
            .map_err(usvg::Error::ParsingFailed)?;
        if expanded(&document) > MAX_NODES {
            return Err(usvg::Error::ElementsLimitReached);
        }
        usvg::Tree::from_xmltree(&document, &options)
    }))
    .map_err(|_| Error::Failed)?;
    let tree = parsed.map_err(|error| match error {
        usvg::Error::InvalidSize => Error::NoSize,
        usvg::Error::ElementsLimitReached => Error::TooManyNodes,
        usvg::Error::ParsingFailed(error) => {
            let at = error.pos();
            Error::Malformed {
                line: at.row,
                column: at.col,
                message: error.to_string(),
            }
        }
        _ => Error::Failed,
    })?;
    let mut nodes = 0;
    let mut texts = 0;
    count(tree.root(), &mut nodes, &mut texts);
    if nodes > MAX_NODES {
        return Err(Error::TooManyNodes);
    }
    let size = tree.size();
    Ok(Svg {
        outline: Outline {
            width: size.width(),
            height: size.height(),
            elements: elements(source),
            nodes,
            texts,
        },
        tree,
    })
}

fn count(group: &usvg::Group, nodes: &mut usize, texts: &mut usize) {
    for node in group.children() {
        *nodes += 1;
        match node {
            usvg::Node::Group(group) => count(group, nodes, texts),
            usvg::Node::Text(_) => *texts += 1,
            _ => {}
        }
        if *nodes > MAX_NODES {
            return;
        }
    }
}

/// Start and empty tags after the first, outside comments, CDATA,
/// processing instructions, and declarations.
fn elements(source: &str) -> usize {
    let mut tags = 0usize;
    let mut at = 0;
    while let Some(offset) = source[at..].find('<') {
        let start = at + offset;
        let rest = &source[start..];
        let skip = |pattern: &str| rest.find(pattern).map(|end| end + pattern.len());
        let length = if rest.starts_with("<!--") {
            skip("-->")
        } else if rest.starts_with("<![CDATA[") {
            skip("]]>")
        } else if rest.starts_with("<?") {
            skip("?>")
        } else if rest.starts_with("<!") || rest.starts_with("</") {
            skip(">")
        } else {
            tags += 1;
            tag_end(rest).map(|(length, _)| length)
        };
        let Some(length) = length else {
            break;
        };
        at = start + length;
    }
    tags.saturating_sub(1)
}

/// One past the most [`expanded`] counts.
const EXPANDED_CAP: usize = MAX_NODES + 1;

/// The elements `document` holds once every `<use>` is replaced by a copy
/// of what it links to, and every marker is drawn at each vertex of the
/// shapes that may take one; counted no further than just past
/// [`MAX_NODES`]. A `<use>` that links to something holding itself copies
/// nothing there, as resvg draws it.
fn expanded(document: &roxmltree::Document) -> usize {
    let mut ids = HashMap::new();
    let mut markers = Vec::new();
    let mut styled = false;
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        if let Some(id) = node.attribute("id") {
            ids.entry(id).or_insert(node);
        }
        match node.tag_name().name() {
            "marker" => markers.push(node),
            // A style sheet may give any shape a marker.
            "style" => {
                styled |= node
                    .descendants()
                    .filter_map(|text| text.text())
                    .any(|text| text.contains("marker"));
            }
            _ => {}
        }
    }
    let plain = expand(document, &ids, &|_| 1);
    let total = |sizes: &[usize]| sizes[document.root_element().id().get_usize()];
    // The largest marker, as drawn once.
    let marker = markers
        .iter()
        .map(|node| plain[node.id().get_usize()])
        .max()
        .unwrap_or(0);
    if marker == 0 || total(&plain) > MAX_NODES {
        return total(&plain);
    }
    let weight = |node: roxmltree::Node| {
        let marked = styled
            || node.attributes().any(|attribute| {
                attribute.name().starts_with("marker")
                    || (attribute.name() == "style" && attribute.value().contains("marker"))
            });
        if marked {
            vertices(node).saturating_mul(marker).saturating_add(1)
        } else {
            1
        }
    };
    total(&expand(document, &ids, &weight))
}

/// Each element's size by its node's index: its `weight`, its children's
/// sizes, and, for a `<use>`, the size of what it links to; none counted
/// past [`EXPANDED_CAP`]. Walked with a stack of its own, so neither deep
/// nesting nor a long chain of links can overflow the thread's.
fn expand(
    document: &roxmltree::Document,
    ids: &HashMap<&str, roxmltree::Node>,
    weight: &dyn Fn(roxmltree::Node) -> usize,
) -> Vec<usize> {
    const NEW: u8 = 0;
    const OPEN: u8 = 1;
    const DONE: u8 = 2;
    let count = document.root().descendants().count();
    let mut sizes = vec![0usize; count];
    let mut states = vec![NEW; count];
    let link = |node: roxmltree::Node| {
        if node.tag_name().name() != "use" {
            return None;
        }
        let href = node
            .attributes()
            .find(|attribute| attribute.name() == "href")?
            .value();
        ids.get(href.trim().strip_prefix('#')?).copied()
    };
    let mut stack = vec![(document.root_element(), false)];
    while let Some((node, leaving)) = stack.pop() {
        let at = node.id().get_usize();
        if leaving {
            // A child or a link still open holds this element: it counts
            // as nothing, since the copy stops there.
            let mut size = weight(node).min(EXPANDED_CAP);
            for child in node.children().filter(roxmltree::Node::is_element) {
                size = size.saturating_add(sizes[child.id().get_usize()]);
            }
            if let Some(target) = link(node) {
                size = size.saturating_add(sizes[target.id().get_usize()]);
            }
            sizes[at] = size.min(EXPANDED_CAP);
            states[at] = DONE;
            continue;
        }
        if states[at] != NEW {
            continue;
        }
        states[at] = OPEN;
        stack.push((node, true));
        for child in node.children().filter(roxmltree::Node::is_element) {
            if states[child.id().get_usize()] == NEW {
                stack.push((child, false));
            }
        }
        if let Some(target) = link(node)
            && states[target.id().get_usize()] == NEW
        {
            stack.push((target, false));
        }
    }
    sizes
}

/// About how many vertices a shape has, where a marker may be drawn: a
/// `<line>`'s two, or half the numbers of a `d` or `points` attribute.
fn vertices(node: roxmltree::Node) -> usize {
    let numbers = |text: &str| {
        let bytes = text.as_bytes();
        let part = |byte: u8| byte.is_ascii_digit() || byte == b'.';
        (0..bytes.len())
            .filter(|&at| part(bytes[at]) && (at == 0 || !part(bytes[at - 1])))
            .count()
    };
    let path = node
        .attribute("d")
        .or_else(|| node.attribute("points"))
        .map_or(0, |data| numbers(data) / 2 + 1);
    if node.tag_name().name() == "line" {
        path.max(2)
    } else {
        path
    }
}

/// The name after a qualified name's prefix.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

impl Svg {
    pub fn outline(&self) -> Outline {
        self.outline
    }

    /// The drawing's texts as laid out and its shapes outside the canvas.
    pub fn report(&self) -> Report {
        let mut report = Report::default();
        let size = self.tree.size();
        // Groups to walk, and whether a clip path or a mask cuts them.
        let mut groups = vec![(self.tree.root(), false)];
        while let Some((group, cut)) = groups.pop() {
            // The stack reverses the order, so texts are sorted after.
            for node in group.children() {
                match node {
                    usvg::Node::Group(inner) => {
                        let cut = cut || inner.clip_path().is_some() || inner.mask().is_some();
                        groups.push((inner, cut));
                    }
                    usvg::Node::Text(text) => {
                        if report.texts.len() < MAX_REPORTED_TEXTS
                            && let Some(found) = text_box(text)
                        {
                            report.texts.push(found);
                        }
                    }
                    usvg::Node::Path(_) | usvg::Node::Image(_) => {
                        if cut {
                            continue;
                        }
                        report.shapes += 1;
                        let outside = node.abs_stroke_bounding_box().is_some_and(|bounds| {
                            bounds.right() <= 0.0
                                || bounds.bottom() <= 0.0
                                || bounds.left() >= size.width()
                                || bounds.top() >= size.height()
                        });
                        if outside {
                            report.hidden_shapes += 1;
                        }
                    }
                }
            }
        }
        // Reading order: down, then across.
        report.texts.sort_by(|a, b| {
            a.top
                .total_cmp(&b.top)
                .then(a.left.total_cmp(&b.left))
                .then_with(|| a.label.cmp(&b.label))
        });
        report
    }

    /// [`Svg::render`], encoded as PNG.
    pub fn render_png(&self, width: u32, height: u32) -> Result<Vec<u8>, Error> {
        let raster = self.render(width, height)?;
        let Some(mut pixmap) = tiny_skia::Pixmap::new(raster.width, raster.height) else {
            return Err(Error::Failed);
        };
        for (pixel, rgba) in pixmap
            .pixels_mut()
            .iter_mut()
            .zip(raster.rgba.as_chunks::<4>().0)
        {
            let [r, g, b, a] = *rgba;
            *pixel = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
        }
        pixmap.encode_png().map_err(|_| Error::Failed)
    }

    /// Draws the whole drawing stretched to `width` by `height` pixels; use
    /// [`fit`] to keep its proportions. A size over [`MAX_PIXELS`], or a
    /// zero side, draws a 1 by 1 transparent raster.
    pub fn render(&self, width: u32, height: u32) -> Result<Raster, Error> {
        let blank = || Raster {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Ok(blank());
        }
        let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
            return Ok(blank());
        };
        let size = self.tree.size();
        let transform = tiny_skia::Transform::from_scale(
            width as f32 / size.width(),
            height as f32 / size.height(),
        );
        catch_unwind(AssertUnwindSafe(|| {
            resvg::render(&self.tree, transform, &mut pixmap.as_mut());
        }))
        .map_err(|_| Error::Failed)?;
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for pixel in pixmap.pixels() {
            let colour = pixel.demultiply();
            rgba.extend([colour.red(), colour.green(), colour.blue(), colour.alpha()]);
        }
        Ok(Raster {
            width,
            height,
            rgba,
        })
    }
}

/// A text's label, size, and box; `None` when it lays out to nothing.
fn text_box(text: &usvg::Text) -> Option<TextBox> {
    let bounds = text.abs_bounding_box();
    let finite = [bounds.left(), bounds.top(), bounds.right(), bounds.bottom()]
        .iter()
        .all(|value| value.is_finite());
    if !finite || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return None;
    }
    let transform = text.abs_transform();
    let scale = (transform.sx * transform.sy - transform.kx * transform.ky)
        .abs()
        .sqrt();
    let size = text
        .chunks()
        .iter()
        .flat_map(|chunk| chunk.spans())
        .map(|span| span.font_size().get() * scale)
        .filter(|size| size.is_finite())
        .fold(f32::INFINITY, f32::min);
    let mut label = String::new();
    let mut kept = 0;
    let mut spaced = true;
    'chunks: for chunk in text.chunks() {
        for c in chunk.text().chars() {
            let c = if c.is_whitespace() || c.is_control() {
                ' '
            } else {
                c
            };
            if c == ' ' && spaced {
                continue;
            }
            spaced = c == ' ';
            if kept == LABEL_CHARS {
                label.push('…');
                break 'chunks;
            }
            label.push(c);
            kept += 1;
        }
        if !spaced {
            label.push(' ');
            spaced = true;
        }
    }
    Some(TextBox {
        label: label.trim().to_owned(),
        size: if size.is_finite() { size } else { 0.0 },
        left: bounds.left(),
        top: bounds.top(),
        right: bounds.right(),
        bottom: bounds.bottom(),
    })
}

/// The cells an image is drawn in at most: [`LARGE_CELLS`] when `large`,
/// else [`CELLS`].
pub fn cells(large: bool) -> (u32, u32) {
    if large { LARGE_CELLS } else { CELLS }
}

/// The largest pixel size within `max_width` by `max_height` that keeps a
/// drawing of `width` by `height` units in proportion, for pixels
/// `aspect` times as tall as they are wide; each side at least 1.
pub fn fit(width: f32, height: f32, max_width: u32, max_height: u32, aspect: f32) -> (u32, u32) {
    if !(width > 0.0 && height > 0.0 && aspect > 0.0) || max_width == 0 || max_height == 0 {
        return (max_width.max(1), max_height.max(1));
    }
    // Pixels wide per pixel tall the drawing needs.
    let ratio = width / height * aspect;
    let mut w = max_width as f32;
    let mut h = w / ratio;
    if h > max_height as f32 {
        h = max_height as f32;
        w = h * ratio;
    }
    let clamp = |value: f32, max: u32| (value.round() as u32).clamp(1, max);
    (clamp(w, max_width), clamp(h, max_height))
}

/// Documents that parse, made from a source still arriving: the source up
/// to each of its last [`REPAIRS`] complete tags, the latest first, with
/// the elements still open there closed. Empty until the root `<svg>` tag
/// is complete. A complete document gives itself first.
pub fn repairs(source: &str) -> Vec<String> {
    let scan = scan(source);
    scan.ends
        .iter()
        .rev()
        .filter(|end| end.rooted)
        .map(|end| {
            let mut repaired = source[..end.at].to_owned();
            for name in end.open.iter().rev() {
                repaired.push_str("</");
                repaired.push_str(name);
                repaired.push('>');
            }
            repaired
        })
        .collect()
}

/// A place a complete tag ends, and the elements open there.
#[derive(Debug, Clone)]
struct End {
    at: usize,
    open: Vec<String>,
    /// The root element has started by here.
    rooted: bool,
}

struct Scan {
    /// The last [`REPAIRS`] places, in order.
    ends: Vec<End>,
}

/// The name of the first element, once its tag's name is complete.
fn first_element(source: &str) -> Option<&str> {
    let mut at = 0;
    while let Some(offset) = source[at..].find('<') {
        let start = at + offset;
        let rest = &source[start..];
        let skip = if rest.starts_with("<!--") {
            rest.find("-->").map(|end| end + 3)
        } else if rest.starts_with("<?") {
            rest.find("?>").map(|end| end + 2)
        } else if rest.starts_with("<!") {
            rest.find('>').map(|end| end + 1)
        } else {
            let name = tag_name(&rest[1..]);
            return (!name.is_empty() && rest.len() > 1 + name.len()).then_some(name);
        };
        at = start + skip?;
    }
    None
}

/// The tag name at the start of `text`.
fn tag_name(text: &str) -> &str {
    let end = text
        .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '<'))
        .unwrap_or(text.len());
    &text[..end]
}

/// Whether `name` is an XML name in ASCII: a letter, `_`, or `:`, then
/// those, digits, `-`, and `.`. Names outside ASCII stop the scan, as if
/// malformed; the parser still reads the whole source.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | ':'))
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-' | '.'))
}

/// Walks the source's markup, keeping the last few complete tags' ends. It
/// stops at the first markup that is not complete; malformed markup is
/// left for the parser to reject.
fn scan(source: &str) -> Scan {
    let mut ends: Vec<End> = Vec::new();
    let mut open: Vec<String> = Vec::new();
    let mut rooted = false;
    let mut push = |at: usize, open: &[String], rooted: bool| {
        if ends.len() == REPAIRS {
            ends.remove(0);
        }
        ends.push(End {
            at,
            open: open.to_vec(),
            rooted,
        });
    };
    let mut at = 0;
    while let Some(offset) = source[at..].find('<') {
        let start = at + offset;
        let rest = &source[start..];
        let close = |pattern: &str| rest.find(pattern).map(|end| start + end + pattern.len());
        let end = if rest.starts_with("<!--") {
            close("-->")
        } else if rest.starts_with("<![CDATA[") {
            close("]]>")
        } else if rest.starts_with("<?") {
            close("?>")
        } else if rest.starts_with("<!") {
            close(">")
        } else if let Some(name) = rest.strip_prefix("</") {
            let name = tag_name(name);
            let end = close(">");
            if end.is_some() {
                if open.last().is_some_and(|last| last == name) {
                    open.pop();
                } else {
                    // Malformed: stop where the parser will complain.
                    break;
                }
            }
            end
        } else {
            let name = tag_name(&rest[1..]);
            if !name.is_empty() && !is_name(name) {
                // Malformed: stop where the parser will complain.
                break;
            }
            match tag_end(rest) {
                Some((length, empty)) if !name.is_empty() => {
                    if !empty {
                        open.push(name.to_owned());
                    }
                    if !rooted && local(name) == "svg" {
                        rooted = true;
                    }
                    Some(start + length)
                }
                _ => None,
            }
        };
        let Some(end) = end else {
            break;
        };
        if rooted {
            push(end, &open, rooted);
        }
        if rooted && open.is_empty() {
            // The root has closed.
            break;
        }
        at = end;
    }
    Scan { ends }
}

/// The length of the start tag at the start of `text`, through its `>`,
/// and whether it is empty (`/>`); `None` while it is incomplete.
fn tag_end(text: &str) -> Option<(usize, bool)> {
    let mut quote: Option<char> = None;
    let mut previous = '<';
    for (at, c) in text.char_indices().skip(1) {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '>' => return Some((at + 1, previous == '/')),
                // Another tag starts inside this one: malformed.
                '<' => return None,
                _ => {}
            },
        }
        previous = c;
    }
    None
}

#[cfg(test)]
mod property_tests;
#[cfg(test)]
mod tests;
