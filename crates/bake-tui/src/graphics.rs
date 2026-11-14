//! SVG images shown sharp, over the half-block cells the view draws them
//! in, on terminals that show images.
//!
//! The view lists the finished SVG images wholly on screen each frame
//! ([`State::svg_images`]); this module draws each at the terminal's pixel
//! size and places it over its cells, in one of three protocols:
//!
//! - **Kitty** graphics (kitty, Ghostty, WezTerm with `BAKE_IMAGES=kitty`):
//!   PNG, sent once per image and placed by id; placements sit above the
//!   text, so a frame whose images moved deletes every placement and places
//!   them again.
//! - **iTerm2** inline images (iTerm2, WezTerm): PNG drawn into the cells.
//! - **Sixel** (Windows Terminal, foot, mlterm): palette pixels drawn into
//!   the cells. Sixel cannot scale, so it needs the cell's size in pixels:
//!   from the window size the terminal reports, or, on Unix, its answer to
//!   `CSI 16 t` at startup. Without one, Sixel stays off.
//!
//! iTerm2 and Sixel images are drawn into the cells, so the cells' own
//! redraw would leave them half drawn or stale: a frame whose images moved
//! or changed repaints the whole screen first. A frame whose images are as
//! before writes nothing.
//!
//! Detection reads only the environment; `BAKE_IMAGES` chooses `kitty`,
//! `iterm`, `sixel`, or `off`. Inside tmux or screen, which do not pass
//! images through by default, images are off unless chosen.

use std::collections::HashMap;
use std::io::{self, Write};

use bake_tui_view::state::ImagePlacement;
use base64::Engine as _;

/// Default cell size in pixels where the terminal scales the image to the
/// cells it is given, so the exact size only sets the drawing's resolution.
const SCALED_CELL: (u16, u16) = (10, 20);
/// Encoded images kept, the latest drawn last.
const CACHED: usize = 8;
/// Base64 bytes a Kitty graphics command carries.
const KITTY_CHUNK: usize = 4096;
/// Colours a Sixel image takes.
const SIXEL_COLOURS: usize = 255;

/// The pixels an image of `columns` by `rows` cells of `cell` pixels is
/// drawn at. Kitty and iTerm2 scale what they are given to the cells, so
/// a size past [`bake_svg::MAX_PIXELS`], as large cells give a large
/// image, is halved until it fits; Sixel cannot scale, so there it is
/// `None`.
fn pixel_size(protocol: Protocol, cell: (u16, u16), columns: u16, rows: u16) -> Option<(u32, u32)> {
    let mut width = u32::from(columns) * u32::from(cell.0);
    let mut height = u32::from(rows) * u32::from(cell.1);
    while u64::from(width) * u64::from(height) > bake_svg::MAX_PIXELS {
        if protocol == Protocol::Sixel {
            return None;
        }
        width = (width / 2).max(1);
        height = (height / 2).max(1);
    }
    Some((width, height))
}

/// How the terminal shows images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Iterm,
    Sixel,
}

impl Protocol {
    /// The protocol the environment names: `BAKE_IMAGES` first, then the
    /// terminal's own variables.
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        match env("BAKE_IMAGES").as_deref().map(str::trim) {
            Some("kitty") => return Some(Self::Kitty),
            Some("iterm") => return Some(Self::Iterm),
            Some("sixel") => return Some(Self::Sixel),
            Some("off") => return None,
            _ => {}
        }
        if env("TMUX").is_some() || env("STY").is_some() {
            return None;
        }
        let term = env("TERM").unwrap_or_default();
        let program = env("TERM_PROGRAM").unwrap_or_default();
        if env("KITTY_WINDOW_ID").is_some()
            || term.contains("kitty")
            || term.contains("ghostty")
            || program == "ghostty"
        {
            Some(Self::Kitty)
        } else if program == "iTerm.app" || program == "WezTerm" {
            Some(Self::Iterm)
        } else if env("WT_SESSION").is_some()
            || term.starts_with("foot")
            || term.starts_with("mlterm")
        {
            Some(Self::Sixel)
        } else {
            None
        }
    }

    /// Whether the image is drawn into the cells, so the cells' redraw can
    /// leave it behind.
    fn in_cells(self) -> bool {
        self != Self::Kitty
    }
}

/// The images placed, and what is needed to place them.
pub struct Graphics {
    protocol: Protocol,
    /// A cell's width and height in pixels.
    cell: (u16, u16),
    /// What the last frame placed: key, column, row.
    placed: Vec<(u64, u16, u16)>,
    /// Encoded images by key, size, and protocol detail.
    cache: Vec<(u64, Vec<u8>)>,
    /// Kitty image ids already sent, by key.
    sent: HashMap<u64, u32>,
    next_id: u32,
}

impl Graphics {
    /// Images in `protocol` with cells of `cell` pixels; Sixel needs a
    /// known size, so `None` turns it off.
    pub fn new(protocol: Protocol, cell: Option<(u16, u16)>) -> Option<Self> {
        let cell = match (protocol, cell) {
            (_, Some(cell)) if cell.0 > 0 && cell.1 > 0 => cell,
            (Protocol::Sixel, _) => return None,
            _ => SCALED_CELL,
        };
        Some(Self {
            protocol,
            cell,
            placed: Vec::new(),
            cache: Vec::new(),
            sent: HashMap::new(),
            next_id: 1,
        })
    }

    /// Whether `images` differ from the last frame's, so they need placing,
    /// and whether the screen must be repainted before they are.
    pub fn plan(&self, images: &[ImagePlacement]) -> Plan {
        let now: Vec<(u64, u16, u16)> = images
            .iter()
            .map(|image| (image.image.key, image.x, image.y))
            .collect();
        if now == self.placed {
            Plan::Keep
        } else if self.protocol.in_cells() && !self.placed.is_empty() {
            Plan::Repaint
        } else {
            Plan::Place
        }
    }

    /// Forgets what was placed, after the screen was cleared.
    pub fn forget(&mut self) {
        self.placed.clear();
    }

    /// Places `images`, the cursor saved and restored around them.
    pub fn place(&mut self, images: &[ImagePlacement], out: &mut impl Write) -> io::Result<()> {
        out.write_all(b"\x1b7")?;
        if self.protocol == Protocol::Kitty {
            out.write_all(b"\x1b_Ga=d,d=a,q=2\x1b\\")?;
        }
        for placement in images {
            write!(out, "\x1b[{};{}H", placement.y + 1, placement.x + 1)?;
            self.write_image(placement, out)?;
        }
        out.write_all(b"\x1b8")?;
        out.flush()?;
        self.placed = images
            .iter()
            .map(|image| (image.image.key, image.x, image.y))
            .collect();
        Ok(())
    }

    /// Removes every placement, before the terminal is given back.
    pub fn clear(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.protocol == Protocol::Kitty && !self.sent.is_empty() {
            out.write_all(b"\x1b_Ga=d,d=A,q=2\x1b\\")?;
            out.flush()?;
        }
        self.placed.clear();
        self.sent.clear();
        Ok(())
    }

    fn write_image(&mut self, placement: &ImagePlacement, out: &mut impl Write) -> io::Result<()> {
        let image = &placement.image;
        let (columns, rows) = (image.columns, image.rows);
        let Some((width, height)) = pixel_size(self.protocol, self.cell, columns, rows) else {
            // Too many pixels for Sixel: the cells stay as drawn.
            return Ok(());
        };
        match self.protocol {
            Protocol::Kitty => {
                if let Some(id) = self.sent.get(&image.key) {
                    return write!(out, "\x1b_Ga=p,i={id},c={columns},r={rows},C=1,q=2\x1b\\");
                }
                let Ok(png) = image.svg.render_png(width, height) else {
                    return Ok(());
                };
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1).max(1);
                self.sent.insert(image.key, id);
                out.write_all(&kitty(&png, id, columns, rows))
            }
            Protocol::Iterm => {
                let encoded = self.cached(image.key, || {
                    image
                        .svg
                        .render_png(width, height)
                        .ok()
                        .map(|png| iterm(&png, columns, rows))
                });
                out.write_all(&encoded)
            }
            Protocol::Sixel => {
                let encoded = self.cached(image.key, || {
                    image
                        .svg
                        .render(width, height)
                        .ok()
                        .map(|raster| sixel(&raster.rgba, raster.width, raster.height))
                });
                out.write_all(&encoded)
            }
        }
    }

    fn cached(&mut self, key: u64, encode: impl FnOnce() -> Option<Vec<u8>>) -> Vec<u8> {
        if let Some((_, encoded)) = self.cache.iter().find(|(cached, _)| *cached == key) {
            return encoded.clone();
        }
        let encoded = encode().unwrap_or_default();
        if self.cache.len() == CACHED {
            self.cache.remove(0);
        }
        self.cache.push((key, encoded.clone()));
        encoded
    }
}

/// What a frame's images need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// They are as placed.
    Keep,
    /// Place them over the frame as drawn.
    Place,
    /// Repaint the screen, then place them.
    Repaint,
}

/// A Kitty graphics transmission of `png` with `id`, placed over `columns`
/// by `rows` cells at the cursor, which stays where it is.
pub fn kitty(png: &[u8], id: u32, columns: u16, rows: u16) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(KITTY_CHUNK).collect();
    let mut out = Vec::with_capacity(encoded.len() + 64 * chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,f=100,i={id},c={columns},r={rows},C=1,q=2,m={more};").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// An iTerm2 inline image of `png`, stretched over `columns` by `rows`
/// cells at the cursor.
pub fn iterm(png: &[u8], columns: u16, rows: u16) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    format!(
        "\x1b]1337;File=inline=1;size={};width={columns};height={rows};preserveAspectRatio=0:{encoded}\x07",
        png.len()
    )
    .into_bytes()
}

/// A Sixel image of straight RGBA pixels: at most [`SIXEL_COLOURS`]
/// colours, the most frequent at 5 bits a channel, each pixel the nearest
/// after a 4 by 4 ordered dither within a step, so gradients do not band;
/// pixels less than half covered are left unset, so the cell shows.
pub fn sixel(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (width, height) = (width as usize, height as usize);
    let shown = |at: usize| rgba[at * 4 + 3] >= 128;
    let bin = |at: usize| {
        let channel = |offset: usize| usize::from(rgba[at * 4 + offset] >> 3);
        (channel(0) << 10) | (channel(1) << 5) | channel(2)
    };
    let pixels = width * height;
    let mut counts = vec![0u32; 1 << 15];
    for at in (0..pixels).filter(|at| shown(*at)) {
        counts[bin(at)] += 1;
    }
    let mut bins: Vec<usize> = (0..counts.len()).filter(|bin| counts[*bin] > 0).collect();
    bins.sort_by(|a, b| counts[*b].cmp(&counts[*a]).then(a.cmp(b)));
    bins.truncate(SIXEL_COLOURS);
    let colour = |bin: usize| {
        let channel = |shift: usize| ((bin >> shift) & 31) * 255 / 31;
        [channel(10), channel(5), channel(0)]
    };
    let palette: Vec<[usize; 3]> = bins.iter().map(|bin| colour(*bin)).collect();
    // Each bin's nearest palette entry, found once.
    let mut nearest: HashMap<usize, usize> = HashMap::new();
    // Bayer's 4 by 4 matrix: thresholds 0 to 15, spread so neighbours differ.
    const BAYER: [[i16; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let dithered = |at: usize| {
        let (x, y) = (at % width, at / width);
        // From 0 to 7 levels, under one 5-bit step of 8: a value a fraction
        // of the way up its step rounds up in that fraction of pixels.
        let offset = BAYER[y % 4][x % 4] / 2;
        let channel = |c: usize| {
            let value = (i16::from(rgba[at * 4 + c]) + offset).clamp(0, 255);
            (value as usize) >> 3
        };
        (channel(0) << 10) | (channel(1) << 5) | channel(2)
    };
    let mut index = vec![usize::MAX; pixels];
    for at in (0..pixels).filter(|at| shown(*at)) {
        let key = dithered(at);
        let found = *nearest.entry(key).or_insert_with(|| {
            let [r, g, b] = colour(key);
            palette
                .iter()
                .enumerate()
                .min_by_key(|(_, [pr, pg, pb])| {
                    r.abs_diff(*pr).pow(2) + g.abs_diff(*pg).pow(2) + b.abs_diff(*pb).pow(2)
                })
                .map_or(0, |(index, _)| index)
        });
        index[at] = found;
    }
    let mut out = Vec::new();
    // P2 = 1: unset pixels keep what is under them.
    out.extend_from_slice(format!("\x1bP0;1;0q\"1;1;{width};{height}").as_bytes());
    for (number, [r, g, b]) in palette.iter().enumerate() {
        out.extend_from_slice(
            format!(
                "#{number};2;{};{};{}",
                r * 100 / 255,
                g * 100 / 255,
                b * 100 / 255
            )
            .as_bytes(),
        );
    }
    for band in (0..height).step_by(6) {
        let mut used: Vec<usize> = Vec::new();
        for y in band..(band + 6).min(height) {
            for x in 0..width {
                let colour = index[y * width + x];
                if colour != usize::MAX && !used.contains(&colour) {
                    used.push(colour);
                }
            }
        }
        for (k, colour) in used.iter().enumerate() {
            if k > 0 {
                out.push(b'$');
            }
            out.extend_from_slice(format!("#{colour}").as_bytes());
            let mut run: Option<(u8, usize)> = None;
            let flush = |run: Option<(u8, usize)>, out: &mut Vec<u8>| {
                if let Some((glyph, count)) = run {
                    if count > 3 {
                        out.extend_from_slice(format!("!{count}").as_bytes());
                        out.push(glyph);
                    } else {
                        out.extend(std::iter::repeat_n(glyph, count));
                    }
                }
            };
            for x in 0..width {
                let mut bits = 0u8;
                for dy in 0..6 {
                    let y = band + dy;
                    if y < height && index[y * width + x] == *colour {
                        bits |= 1 << dy;
                    }
                }
                let glyph = 63 + bits;
                run = match run {
                    Some((last, count)) if last == glyph => Some((last, count + 1)),
                    other => {
                        flush(other, &mut out);
                        Some((glyph, 1))
                    }
                };
            }
            flush(run, &mut out);
        }
        out.push(b'-');
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// A cell's size in pixels from the window size the terminal reports, when
/// it reports one.
pub fn reported_cell() -> Option<(u16, u16)> {
    let size = crossterm::terminal::window_size().ok()?;
    if size.width == 0 || size.height == 0 || size.columns == 0 || size.rows == 0 {
        return None;
    }
    Some((size.width / size.columns, size.height / size.rows))
}

/// Asks the terminal for its cell size (`CSI 16 t`) and waits up to
/// `wait` for the answer, `CSI 6 ; height ; width t`. Run once, in raw mode,
/// before the input reader starts; bytes read that are not the answer are
/// dropped.
#[cfg(unix)]
pub fn query_cell(wait: std::time::Duration) -> Option<(u16, u16)> {
    use std::os::fd::AsRawFd;
    let mut out = io::stdout();
    out.write_all(b"\x1b[16t").ok()?;
    out.flush().ok()?;
    let fd = io::stdin().as_raw_fd();
    let deadline = std::time::Instant::now() + wait;
    let mut reply = Vec::new();
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
        // SAFETY: `poll` is one valid pollfd for the call's duration.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        if ready <= 0 {
            break;
        }
        let mut buffer = [0u8; 64];
        // SAFETY: `buffer` is valid for writes of its length.
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        let Ok(read) = usize::try_from(read) else {
            break;
        };
        if read == 0 {
            break;
        }
        reply.extend_from_slice(&buffer[..read]);
        if let Some(cell) = parse_cell_reply(&reply) {
            return Some(cell);
        }
        if reply.len() > 256 {
            break;
        }
    }
    None
}

/// The cell size in `CSI 6 ; height ; width t`, anywhere in `bytes`.
pub fn parse_cell_reply(bytes: &[u8]) -> Option<(u16, u16)> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find("\x1b[6;")?;
    let rest = &text[start + 4..];
    let end = rest.find('t')?;
    let (height, width) = rest[..end].split_once(';')?;
    let (height, width) = (height.parse::<u16>().ok()?, width.parse::<u16>().ok()?);
    (height > 0 && width > 0).then_some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_protocol_follows_the_terminal() {
        assert_eq!(
            Protocol::detect(env(&[("TERM", "xterm-kitty")])),
            Some(Protocol::Kitty)
        );
        assert_eq!(
            Protocol::detect(env(&[("TERM_PROGRAM", "ghostty")])),
            Some(Protocol::Kitty)
        );
        assert_eq!(
            Protocol::detect(env(&[("TERM_PROGRAM", "iTerm.app")])),
            Some(Protocol::Iterm)
        );
        assert_eq!(
            Protocol::detect(env(&[("TERM_PROGRAM", "WezTerm")])),
            Some(Protocol::Iterm)
        );
        assert_eq!(
            Protocol::detect(env(&[("WT_SESSION", "x"), ("TERM", "xterm-256color")])),
            Some(Protocol::Sixel)
        );
        assert_eq!(
            Protocol::detect(env(&[("TERM", "foot")])),
            Some(Protocol::Sixel)
        );
        assert_eq!(Protocol::detect(env(&[("TERM", "xterm-256color")])), None);
        // tmux does not pass images through unless chosen.
        assert_eq!(
            Protocol::detect(env(&[("TERM", "xterm-kitty"), ("TMUX", "/tmp/t")])),
            None
        );
        assert_eq!(
            Protocol::detect(env(&[("TMUX", "/tmp/t"), ("BAKE_IMAGES", "sixel")])),
            Some(Protocol::Sixel)
        );
        assert_eq!(
            Protocol::detect(env(&[("TERM", "xterm-kitty"), ("BAKE_IMAGES", "off")])),
            None
        );
    }

    // A large image in large cells is drawn at fewer pixels where the
    // terminal scales it, and left to its cells where it cannot.
    #[test]
    fn a_size_past_the_pixel_limit_is_scaled_or_left_out() {
        assert_eq!(
            pixel_size(Protocol::Kitty, (10, 20), 144, 32),
            Some((1440, 640))
        );
        assert_eq!(
            pixel_size(Protocol::Sixel, (10, 20), 144, 32),
            Some((1440, 640))
        );
        assert_eq!(
            pixel_size(Protocol::Kitty, (24, 48), 144, 32),
            Some((1728, 768))
        );
        assert_eq!(
            pixel_size(Protocol::Iterm, (24, 48), 144, 32),
            Some((1728, 768))
        );
        assert_eq!(pixel_size(Protocol::Sixel, (24, 48), 144, 32), None);
    }

    #[test]
    fn sixel_needs_the_cell_size() {
        assert!(Graphics::new(Protocol::Sixel, None).is_none());
        assert!(Graphics::new(Protocol::Sixel, Some((0, 18))).is_none());
        assert_eq!(
            Graphics::new(Protocol::Sixel, Some((9, 18))).unwrap().cell,
            (9, 18)
        );
        assert_eq!(
            Graphics::new(Protocol::Kitty, None).unwrap().cell,
            SCALED_CELL
        );
    }

    #[test]
    fn the_cell_reply_is_read_among_other_bytes() {
        assert_eq!(parse_cell_reply(b"a\x1b[6;19;9tb"), Some((9, 19)));
        assert_eq!(parse_cell_reply(b"\x1b[6;0;9t"), None);
        assert_eq!(parse_cell_reply(b"\x1b[6;19"), None);
        assert_eq!(parse_cell_reply(b"\x1b[4;400;800t"), None);
    }

    #[test]
    fn kitty_sends_chunks_and_keeps_the_cursor() {
        let png = vec![7u8; 4000];
        let sent = String::from_utf8(kitty(&png, 3, 12, 5)).unwrap();
        let commands: Vec<&str> = sent
            .split("\x1b\\")
            .filter(|part| !part.is_empty())
            .collect();
        // 4000 bytes are 5336 in base64: two chunks.
        assert_eq!(commands.len(), 2);
        assert!(commands[0].starts_with("\x1b_Ga=T,f=100,i=3,c=12,r=5,C=1,q=2,m=1;"));
        assert!(commands[1].starts_with("\x1b_Gm=0;"));
        let body: usize = commands
            .iter()
            .map(|c| c.len() - c.find(';').unwrap() - 1)
            .sum();
        assert_eq!(body, 5336);
    }

    #[test]
    fn iterm_stretches_over_its_cells() {
        let sent = String::from_utf8(iterm(b"abc", 4, 2)).unwrap();
        assert_eq!(
            sent,
            "\x1b]1337;File=inline=1;size=3;width=4;height=2;preserveAspectRatio=0:YWJj\x07"
        );
    }

    #[test]
    fn sixel_bands_runs_and_unset_pixels() {
        // 5 by 7: red, but the last column's top pixel is unset.
        let mut rgba = Vec::new();
        for y in 0..7 {
            for x in 0..5 {
                rgba.extend(if x == 4 && y == 0 {
                    [0, 0, 0, 0]
                } else {
                    [255, 0, 0, 255]
                });
            }
        }
        let sent = String::from_utf8(sixel(&rgba, 5, 7)).unwrap();
        // Band one: 4 full columns (`~`, all six bits), then one without
        // its top pixel (`}`); band two: one row, bit 0, in all five.
        assert_eq!(sent, "\x1bP0;1;0q\"1;1;5;7#0;2;100;0;0#0!4~}-#0!5@-\x1b\\");
        // Nothing shown: a palette of none, and empty bands.
        let blank = String::from_utf8(sixel(&[0; 4 * 2], 2, 1)).unwrap();
        assert_eq!(blank, "\x1bP0;1;0q\"1;1;2;1-\x1b\\");
    }

    #[test]
    fn sixel_keeps_at_most_its_palette() {
        let mut rgba = Vec::new();
        for value in 0..1024u32 {
            rgba.extend([
                (value % 256) as u8,
                (value / 4) as u8,
                (value * 7 % 256) as u8,
                255,
            ]);
        }
        let sent = String::from_utf8(sixel(&rgba, 32, 32)).unwrap();
        let colours = sent.matches(";2;").count();
        assert!(colours <= SIXEL_COLOURS, "{colours}");
        assert!(sent.ends_with("-\x1b\\"));
    }
}
