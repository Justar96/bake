//! HTML to Markdown for `web_fetch` results.
//!
//! Bake 0.3 converted fetched HTML with turndown and
//! `@joplin/turndown-plugin-gfm` plus the rules in
//! `packages/web/tool-web/src/fetch.ts`: ATX headings, fenced code, `-`
//! bullets, `_` and `**` emphasis, GitHub tables whose cells ignore span
//! counts, and non-visible elements removed (`script`, `style`,
//! `noscript`, `template`, `iframe`, `object`, `embed`, and any element
//! that is `hidden`, `aria-hidden="true"`, a hidden `input`, or styled
//! `display: none` or `visibility: hidden` or `collapse`). This module is
//! Bake's own single-pass converter producing that Markdown: a tokenizer
//! and an explicit element stack, so no input nests a call stack, and the
//! work is linear in the input. Its tests compare it with the 0.3
//! converter's output for the same documents.
//!
//! It is not a full HTML parser: implied end tags follow the common cases
//! (`p`, `li`, `dt`, `dd`, table rows and cells, headings), entities are
//! the common named ones and numeric references, and a `pre` without a
//! `code` child stays a whitespace-preserving block as in turndown.
//!
//! [`exceeds_conversion_depth`] ports 0.3's lexical depth preflight, which
//! omits deeply nested documents (`MAX_CONVERSION_DEPTH`).

/// Nesting depth above which HTML is omitted rather than converted
/// (`MAX_CONVERSION_DEPTH`).
pub const MAX_CONVERSION_DEPTH: usize = 512;

/// Elements that never take a closing tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose contents the preflight skips as text.
const PREFLIGHT_RAW_TEXT: &[&str] = &["script", "style", "noscript"];

fn is_tag_boundary(byte: Option<&u8>) -> bool {
    match byte {
        None => true,
        Some(byte) => matches!(byte, b'>' | b'/') || byte.is_ascii_whitespace(),
    }
}

/// `findRawTextEnd`: the matching end tag of a raw-text element.
fn find_raw_text_end(lower: &str, name: &str, from: usize) -> Option<usize> {
    let prefix = format!("</{name}");
    let mut search = from;
    loop {
        let candidate = search + lower.get(search..)?.find(&prefix)?;
        if is_tag_boundary(lower.as_bytes().get(candidate + prefix.len())) {
            return Some(candidate);
        }
        search = candidate + prefix.len();
    }
}

/// `exceedsConversionDepth`: whether the lexical element stack crosses
/// [`MAX_CONVERSION_DEPTH`]. Closing tags inside comments are ignored,
/// raw-text bodies skipped, quoted `>` respected, and only a closing tag
/// for the current element pops, so malformed input over-counts rather
/// than hiding nesting.
pub fn exceeds_conversion_depth(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut open: Vec<&str> = Vec::new();
    let mut offset = 0usize;
    let mut in_comment = false;
    // Where the next `-->` at or after an offset is; `None` once none is
    // left, so an unterminated comment is not searched again per `<`.
    let mut comment_end: Option<Option<usize>> = None;
    while offset < bytes.len() {
        let start = html[offset..].find('<').map(|index| offset + index);
        if in_comment {
            let end = match comment_end {
                Some(Some(end)) if end >= offset => Some(end),
                Some(None) => None,
                _ => {
                    let found = html[offset..].find("-->").map(|index| offset + index);
                    comment_end = Some(found);
                    found
                }
            };
            if let Some(end) = end
                && start.is_none_or(|start| end < start)
            {
                in_comment = false;
                offset = end + 3;
                continue;
            }
        }
        let Some(start) = start else {
            break;
        };
        if !in_comment && html[start..].starts_with("<!--") {
            in_comment = true;
            offset = start + 4;
            continue;
        }
        let mut cursor = start + 1;
        let closing = bytes.get(cursor) == Some(&b'/');
        if closing {
            cursor += 1;
        }
        let name_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            cursor += 1;
        }
        if cursor == name_start || !bytes[name_start].is_ascii_alphabetic() {
            offset = start + 1;
            continue;
        }
        let name = &lower[name_start..cursor];
        let mut quote: Option<u8> = None;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            cursor += 1;
            match quote {
                Some(open_quote) if byte == open_quote => quote = None,
                Some(_) => {}
                None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                None if byte == b'>' => break,
                None => {}
            }
        }
        if bytes[cursor - 1] != b'>' {
            break;
        }
        if closing {
            if !in_comment && open.last() == Some(&name) {
                open.pop();
            }
        } else {
            let mut last = cursor.checked_sub(2);
            while let Some(index) = last
                && bytes[index].is_ascii_whitespace()
            {
                last = index.checked_sub(1);
            }
            let self_closing = last.is_some_and(|index| bytes[index] == b'/');
            if !VOID_ELEMENTS.contains(&name) && !self_closing {
                open.push(name);
                if open.len() > MAX_CONVERSION_DEPTH {
                    return true;
                }
                if !in_comment && PREFLIGHT_RAW_TEXT.contains(&name) {
                    match find_raw_text_end(&lower, name, cursor) {
                        Some(end) => {
                            offset = end;
                            continue;
                        }
                        None => break,
                    }
                }
            }
        }
        offset = cursor;
    }
    false
}

/// One lexical token.
#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Start {
        name: String,
        attributes: Vec<(String, String)>,
        self_closing: bool,
    },
    End(String),
}

/// Elements whose contents are text up to their end tag.
const RAW_TEXT: &[&str] = &[
    "script", "style", "noscript", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
];

/// The tokens of `html`, in order.
fn tokenize(html: &str) -> Vec<Token> {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut tokens = Vec::new();
    let mut text_start = 0usize;
    let mut cursor = 0usize;
    let flush = |tokens: &mut Vec<Token>, from: usize, to: usize| {
        if to > from {
            tokens.push(Token::Text(decode_entities(&html[from..to])));
        }
    };
    while let Some(found) = html[cursor..].find('<') {
        let start = cursor + found;
        let next = bytes.get(start + 1).copied();
        match next {
            Some(b'!') => {
                flush(&mut tokens, text_start, start);
                let end = if html[start..].starts_with("<!--") {
                    html[start + 4..]
                        .find("-->")
                        .map_or(bytes.len(), |index| start + 4 + index + 3)
                } else {
                    html[start..]
                        .find('>')
                        .map_or(bytes.len(), |index| start + index + 1)
                };
                cursor = end;
                text_start = end;
            }
            Some(b'?') => {
                flush(&mut tokens, text_start, start);
                cursor = html[start..]
                    .find('>')
                    .map_or(bytes.len(), |index| start + index + 1);
                text_start = cursor;
            }
            Some(b'/') if bytes.get(start + 2).is_some_and(u8::is_ascii_alphabetic) => {
                flush(&mut tokens, text_start, start);
                let name_end = name_end(bytes, start + 2);
                let name = lower[start + 2..name_end].to_owned();
                cursor = html[name_end..]
                    .find('>')
                    .map_or(bytes.len(), |index| name_end + index + 1);
                text_start = cursor;
                tokens.push(Token::End(name));
            }
            Some(byte) if byte.is_ascii_alphabetic() => {
                flush(&mut tokens, text_start, start);
                let name_end = name_end(bytes, start + 1);
                let name = lower[start + 1..name_end].to_owned();
                let (attributes, self_closing, after) = attributes(html, name_end);
                cursor = after;
                text_start = after;
                if RAW_TEXT.contains(&name.as_str()) && !self_closing {
                    let end = find_raw_text_end(&lower, &name, after).unwrap_or(bytes.len());
                    let body = &html[after..end];
                    tokens.push(Token::Start {
                        name: name.clone(),
                        attributes,
                        self_closing: false,
                    });
                    if !body.is_empty() {
                        tokens.push(Token::Text(decode_entities(body)));
                    }
                    tokens.push(Token::End(name));
                    cursor = html[end..]
                        .find('>')
                        .map_or(bytes.len(), |index| end + index + 1);
                    text_start = cursor;
                } else {
                    tokens.push(Token::Start {
                        name,
                        attributes,
                        self_closing,
                    });
                }
            }
            _ => cursor = start + 1,
        }
    }
    flush(&mut tokens, text_start, bytes.len());
    tokens
}

fn name_end(bytes: &[u8], from: usize) -> usize {
    let mut end = from;
    while bytes
        .get(end)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'/' | b'>'))
    {
        end += 1;
    }
    end
}

/// A start tag's attributes from `from`, whether it closes itself, and
/// the offset after its `>`.
fn attributes(html: &str, from: usize) -> (Vec<(String, String)>, bool, usize) {
    let bytes = html.as_bytes();
    let mut cursor = from;
    let mut list = Vec::new();
    let mut self_closing = false;
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        match bytes.get(cursor) {
            None => return (list, self_closing, bytes.len()),
            Some(b'>') => return (list, self_closing, cursor + 1),
            Some(b'/') => {
                self_closing = true;
                cursor += 1;
                continue;
            }
            Some(_) => self_closing = false,
        }
        let name_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'>' | b'/'))
        {
            cursor += 1;
        }
        if cursor == name_start {
            // A stray `=`.
            cursor += 1;
            continue;
        }
        let name = html[name_start..cursor].to_ascii_lowercase();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let mut value = String::new();
        if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            match bytes.get(cursor) {
                Some(quote @ (b'"' | b'\'')) => {
                    let quote = char::from(*quote);
                    let start = cursor + 1;
                    let end = html[start..]
                        .find(quote)
                        .map_or(bytes.len(), |index| start + index);
                    value = decode_entities(&html[start..end]);
                    cursor = (end + 1).min(bytes.len());
                }
                _ => {
                    let start = cursor;
                    while bytes
                        .get(cursor)
                        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
                    {
                        cursor += 1;
                    }
                    value = decode_entities(&html[start..cursor]);
                }
            }
        }
        list.push((name, value));
    }
}

/// Named references decoded, the Latin-1 set and common punctuation;
/// others stay as written.
const ENTITIES: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", "\u{a0}"),
    ("copy", "©"),
    ("reg", "®"),
    ("trade", "™"),
    ("hellip", "…"),
    ("mdash", "—"),
    ("ndash", "–"),
    ("lsquo", "‘"),
    ("rsquo", "’"),
    ("ldquo", "“"),
    ("rdquo", "”"),
    ("laquo", "«"),
    ("raquo", "»"),
    ("middot", "·"),
    ("bull", "•"),
    ("euro", "€"),
    ("pound", "£"),
    ("yen", "¥"),
    ("cent", "¢"),
    ("times", "×"),
    ("divide", "÷"),
    ("deg", "°"),
    ("plusmn", "±"),
    ("para", "¶"),
    ("sect", "§"),
    ("shy", "\u{ad}"),
    ("ensp", "\u{2002}"),
    ("emsp", "\u{2003}"),
    ("thinsp", "\u{2009}"),
    ("zwnj", "\u{200c}"),
    ("zwj", "\u{200d}"),
    ("larr", "←"),
    ("rarr", "→"),
    ("uarr", "↑"),
    ("darr", "↓"),
    ("harr", "↔"),
    ("le", "≤"),
    ("ge", "≥"),
    ("ne", "≠"),
    ("minus", "−"),
    ("iexcl", "¡"),
    ("curren", "¤"),
    ("brvbar", "¦"),
    ("uml", "¨"),
    ("ordf", "ª"),
    ("not", "¬"),
    ("macr", "¯"),
    ("sup2", "²"),
    ("sup3", "³"),
    ("acute", "´"),
    ("micro", "µ"),
    ("cedil", "¸"),
    ("sup1", "¹"),
    ("ordm", "º"),
    ("frac14", "¼"),
    ("frac12", "½"),
    ("frac34", "¾"),
    ("iquest", "¿"),
    ("Agrave", "À"),
    ("Aacute", "Á"),
    ("Acirc", "Â"),
    ("Atilde", "Ã"),
    ("Auml", "Ä"),
    ("Aring", "Å"),
    ("AElig", "Æ"),
    ("Ccedil", "Ç"),
    ("Egrave", "È"),
    ("Eacute", "É"),
    ("Ecirc", "Ê"),
    ("Euml", "Ë"),
    ("Igrave", "Ì"),
    ("Iacute", "Í"),
    ("Icirc", "Î"),
    ("Iuml", "Ï"),
    ("ETH", "Ð"),
    ("Ntilde", "Ñ"),
    ("Ograve", "Ò"),
    ("Oacute", "Ó"),
    ("Ocirc", "Ô"),
    ("Otilde", "Õ"),
    ("Ouml", "Ö"),
    ("Oslash", "Ø"),
    ("Ugrave", "Ù"),
    ("Uacute", "Ú"),
    ("Ucirc", "Û"),
    ("Uuml", "Ü"),
    ("Yacute", "Ý"),
    ("THORN", "Þ"),
    ("szlig", "ß"),
    ("agrave", "à"),
    ("aacute", "á"),
    ("acirc", "â"),
    ("atilde", "ã"),
    ("auml", "ä"),
    ("aring", "å"),
    ("aelig", "æ"),
    ("ccedil", "ç"),
    ("egrave", "è"),
    ("eacute", "é"),
    ("ecirc", "ê"),
    ("euml", "ë"),
    ("igrave", "ì"),
    ("iacute", "í"),
    ("icirc", "î"),
    ("iuml", "ï"),
    ("eth", "ð"),
    ("ntilde", "ñ"),
    ("ograve", "ò"),
    ("oacute", "ó"),
    ("ocirc", "ô"),
    ("otilde", "õ"),
    ("ouml", "ö"),
    ("oslash", "ø"),
    ("ugrave", "ù"),
    ("uacute", "ú"),
    ("ucirc", "û"),
    ("uuml", "ü"),
    ("yacute", "ý"),
    ("thorn", "þ"),
    ("yuml", "ÿ"),
];

/// References a document may write without the closing `;`.
const LEGACY_ENTITIES: &[&str] = &["amp", "lt", "gt", "quot", "nbsp"];

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        match decode_reference(rest) {
            Some((decoded, used)) => {
                out.push_str(&decoded);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The reference at the start of `text` (which starts with `&`) and the
/// bytes it spans.
fn decode_reference(text: &str) -> Option<(String, usize)> {
    let body = &text[1..];
    if let Some(numeric) = body.strip_prefix('#') {
        let (digits, radix, skip) = match numeric.strip_prefix(['x', 'X']) {
            Some(hex) => (hex, 16, 3),
            None => (numeric, 10, 2),
        };
        let length = digits
            .bytes()
            .take_while(|byte| byte.is_ascii_hexdigit() && (radix == 16 || byte.is_ascii_digit()))
            .count();
        if length == 0 {
            return None;
        }
        let value = u32::from_str_radix(&digits[..length.min(8)], radix).unwrap_or(u32::MAX);
        let decoded = match value {
            0 => '\u{fffd}',
            value => char::from_u32(value).unwrap_or('\u{fffd}'),
        };
        let semicolon = usize::from(digits.as_bytes().get(length) == Some(&b';'));
        return Some((decoded.to_string(), skip + length + semicolon));
    }
    let length = body.bytes().take_while(u8::is_ascii_alphanumeric).count();
    let name = &body[..length];
    let (_, value) = ENTITIES.iter().find(|(known, _)| *known == name)?;
    if body.as_bytes().get(length) == Some(&b';') {
        Some(((*value).to_owned(), 1 + length + 1))
    } else if LEGACY_ENTITIES.contains(&name) {
        Some(((*value).to_owned(), 1 + length))
    } else {
        None
    }
}

/// Elements turndown treats as blocks.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "body",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frameset",
    "header",
    "hgroup",
    "html",
    "main",
    "menu",
    "nav",
    "p",
    "section",
    "summary",
];

/// Elements whose start closes an open `p`.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
];

/// Elements removed with their contents.
const REMOVED: &[&str] = &[
    "script", "style", "noscript", "template", "iframe", "object", "embed",
];

/// What an open element does with its content.
#[derive(Debug)]
enum Frame {
    /// Nothing of its own.
    Plain,
    /// A block: separated from what surrounds it.
    Block,
    /// A removed element; everything inside is dropped.
    Skip,
    /// A `pre`: whitespace kept; fenced when it holds `code`.
    Pre {
        language: Option<String>,
        fenced: bool,
    },
    /// The buffered content of an element rendered when it closes.
    Buffered(Buffered),
    /// A table and its rows.
    Table(Table),
}

#[derive(Debug)]
enum Buffered {
    Heading(usize),
    Link {
        href: Option<String>,
        title: Option<String>,
    },
    Delimited(&'static str),
    Code,
    Blockquote,
    List {
        ordered: bool,
        next: i64,
    },
    Item,
    Cell {
        heading: bool,
        alignment: Option<String>,
    },
}

#[derive(Debug, Default)]
struct Table {
    rows: Vec<Row>,
}

#[derive(Debug, Default)]
struct Row {
    cells: Vec<Cell>,
    in_head: bool,
}

#[derive(Debug)]
struct Cell {
    text: String,
    heading: bool,
    alignment: Option<String>,
}

struct Open {
    name: String,
    frame: Frame,
}

/// The converter's state: the open elements and the output buffers, the
/// document's first and one more per open [`Frame::Buffered`].
struct Converter {
    open: Vec<Open>,
    buffers: Vec<String>,
    skipping: usize,
    /// How many open elements have each name, so the searches for an
    /// implied end tag cost nothing when no candidate is open.
    counts: std::collections::HashMap<String, usize>,
    /// Open inline `code` elements.
    code: usize,
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The `removeNonVisibleContent` rule.
fn is_removed(name: &str, attributes: &[(String, String)]) -> bool {
    if REMOVED.contains(&name) || attribute(attributes, "hidden").is_some() {
        return true;
    }
    if attribute(attributes, "aria-hidden").is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if name == "input"
        && attribute(attributes, "type").is_some_and(|value| value.eq_ignore_ascii_case("hidden"))
    {
        return true;
    }
    attribute(attributes, "style").is_some_and(|style| {
        style.split(';').any(|declaration| {
            let Some((property, value)) = declaration.split_once(':') else {
                return false;
            };
            let property = property.trim().to_ascii_lowercase();
            let value = value.trim().to_ascii_lowercase();
            let value = value
                .strip_suffix("!important")
                .map_or(value.as_str(), str::trim_end);
            (property == "display" && value == "none")
                || (property == "visibility" && (value == "hidden" || value == "collapse"))
        })
    })
}

/// Separate a block from what came before: no trailing spaces, then a
/// blank line, unless the buffer is empty.
fn block_break(buffer: &mut String) {
    let kept = buffer.trim_end_matches([' ', '\t']).len();
    if kept != buffer.len() && !buffer.ends_with("  ") {
        buffer.truncate(kept);
    }
    if buffer.trim().is_empty() {
        buffer.clear();
        return;
    }
    let newlines = buffer.len() - buffer.trim_end_matches('\n').len();
    for _ in newlines..2 {
        buffer.push('\n');
    }
}

/// turndown's `escape`: Markdown syntax in text read as text. The
/// line-start rules apply where the text node starts.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '`' | '[' | ']' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    if let Some(rest) = out.strip_prefix('-') {
        return format!("\\-{rest}");
    }
    if let Some(rest) = out.strip_prefix("+ ") {
        return format!("\\+ {rest}");
    }
    if out.starts_with('=') || out.starts_with('>') {
        return format!("\\{out}");
    }
    if out.starts_with("~~~") {
        return format!("\\{out}");
    }
    let hashes = out.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&hashes) && out.as_bytes().get(hashes) == Some(&b' ') {
        return format!("\\{out}");
    }
    let digits = out.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && out[digits..].starts_with(". ") {
        return format!("{}\\{}", &out[..digits], &out[digits..]);
    }
    out
}

/// Runs of HTML whitespace as one space.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// Append inline Markdown, dropping a space where a line starts or one
/// already ends the buffer.
fn push_inline(buffer: &mut String, text: &str) {
    let text = if buffer.is_empty() || buffer.ends_with('\n') || buffer.ends_with(' ') {
        text.trim_start_matches(' ')
    } else {
        text
    };
    buffer.push_str(text);
}

/// `renderTableCell` (0.3 `fetch.ts`).
fn render_cell(content: &str, index: usize) -> String {
    let prefix = if index == 0 { "| " } else { " " };
    let mut escaped = String::new();
    let mut pipes = false;
    for c in content
        .trim()
        .replace("\n\r", "<br>")
        .replace('\n', "<br>")
        .chars()
    {
        if c == '|' {
            pipes = true;
            continue;
        }
        if pipes {
            escaped.push_str("\\|");
            pipes = false;
        }
        escaped.push(c);
    }
    if pipes {
        escaped.push_str("\\|");
    }
    let width = super::utf16_len(&escaped);
    for _ in width..3 {
        escaped.push(' ');
    }
    format!("{prefix}{escaped} |")
}

/// `tableBorder`: the separator for a cell's alignment.
fn border(alignment: Option<&str>) -> &'static str {
    match alignment.map(str::to_ascii_lowercase).as_deref() {
        Some("left") => ":---",
        Some("right") => "---:",
        Some("center") => ":---:",
        _ => "---",
    }
}

fn alignment(attributes: &[(String, String)]) -> Option<String> {
    if let Some(align) = attribute(attributes, "align").filter(|align| !align.is_empty()) {
        return Some(align.to_owned());
    }
    let style = attribute(attributes, "style")?;
    style.split(';').find_map(|declaration| {
        let (property, value) = declaration.split_once(':')?;
        (property.trim().eq_ignore_ascii_case("text-align")).then(|| value.trim().to_owned())
    })
}

impl Table {
    /// The table rule of the GFM plugin, with 0.3's row and cell rules.
    fn render(self) -> String {
        let columns = self
            .rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0);
        let single = self.rows.len() == 1 && columns <= 1;
        let mut content = String::new();
        let first_alignments: Vec<Option<String>> = self
            .rows
            .first()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.alignment.clone())
                    .collect()
            })
            .unwrap_or_default();
        for (index, row) in self.rows.into_iter().enumerate() {
            let heading = (row.in_head || index == 0)
                && !row.cells.is_empty()
                && row.cells.iter().all(|cell| cell.heading);
            content.push('\n');
            for (position, cell) in row.cells.iter().enumerate() {
                content.push_str(&render_cell(&cell.text, position));
            }
            if heading {
                content.push('\n');
                for (position, cell) in row.cells.iter().enumerate() {
                    content.push_str(&render_cell(border(cell.alignment.as_deref()), position));
                }
            }
        }
        if single {
            return content;
        }
        let mut lines: Vec<&str> = content
            .split('\n')
            .filter(|line| !line.is_empty())
            .collect();
        let divider = lines
            .get(1)
            .is_some_and(|line| line.contains("| ---") || line.contains("| :---"));
        let mut table = String::new();
        if columns > 0 && !divider {
            table.push('|');
            for _ in 0..columns {
                table.push_str("     |");
            }
            table.push_str("\n|");
            for column in 0..columns {
                let alignment = first_alignments.get(column).cloned().flatten();
                table.push_str(&format!(" {} |", border(alignment.as_deref())));
            }
            table.push('\n');
        }
        lines.retain(|line| !line.is_empty());
        table.push_str(&lines.join("\n"));
        table
    }
}

impl Converter {
    fn new() -> Self {
        Self {
            open: Vec::new(),
            buffers: vec![String::new()],
            skipping: 0,
            counts: std::collections::HashMap::new(),
            code: 0,
        }
    }

    fn push(&mut self, open: Open) {
        *self.counts.entry(open.name.clone()).or_default() += 1;
        if matches!(open.frame, Frame::Buffered(Buffered::Code)) {
            self.code += 1;
        }
        self.open.push(open);
    }

    fn pop(&mut self) -> Option<Open> {
        let open = self.open.pop()?;
        if let Some(count) = self.counts.get_mut(&open.name) {
            *count = count.saturating_sub(1);
        }
        if matches!(open.frame, Frame::Buffered(Buffered::Code)) {
            self.code = self.code.saturating_sub(1);
        }
        Some(open)
    }

    fn count(&self, name: &str) -> usize {
        self.counts.get(name).copied().unwrap_or(0)
    }

    fn top(&mut self) -> &mut String {
        if self.buffers.is_empty() {
            self.buffers.push(String::new());
        }
        let last = self.buffers.len() - 1;
        &mut self.buffers[last]
    }

    fn in_pre(&self) -> Option<bool> {
        if self.count("pre") == 0 {
            return None;
        }
        self.open.iter().rev().find_map(|open| match &open.frame {
            Frame::Pre { fenced, .. } => Some(*fenced),
            _ => None,
        })
    }

    fn in_code(&self) -> bool {
        self.code > 0
    }

    /// The innermost open element named one of `names`, searching no
    /// further than one named one of `boundary`.
    fn find_open(&self, names: &[&str], boundary: &[&str]) -> Option<usize> {
        if names.iter().all(|name| self.count(name) == 0) {
            return None;
        }
        for (index, open) in self.open.iter().enumerate().rev() {
            if names.contains(&open.name.as_str()) {
                return Some(index);
            }
            if boundary.contains(&open.name.as_str()) {
                return None;
            }
        }
        None
    }

    fn close_to(&mut self, index: usize) {
        while self.open.len() > index {
            self.close_top();
        }
    }

    fn text(&mut self, text: &str) {
        if self.skipping > 0 {
            return;
        }
        if let Some(fenced) = self.in_pre() {
            let first = self.top().is_empty()
                && matches!(
                    self.open.last(),
                    Some(Open {
                        frame: Frame::Pre { .. },
                        ..
                    })
                );
            let text = if first {
                text.strip_prefix('\n').unwrap_or(text)
            } else {
                text
            };
            let text = if fenced || self.in_code() {
                text.to_owned()
            } else {
                escape(text)
            };
            self.top().push_str(&text);
            return;
        }
        if self.open.last().is_some_and(|open| {
            matches!(open.frame, Frame::Table(_))
                || matches!(open.name.as_str(), "tr" | "thead" | "tbody" | "tfoot")
        }) {
            return;
        }
        let collapsed = collapse_whitespace(text);
        let code = self.in_code();
        let buffer = self.top();
        let at_line_start = buffer.is_empty() || buffer.ends_with('\n') || buffer.ends_with(' ');
        let trimmed = if at_line_start {
            collapsed.trim_start_matches(' ')
        } else {
            collapsed.as_str()
        };
        if trimmed.is_empty() {
            return;
        }
        let rendered = if code {
            trimmed.to_owned()
        } else if at_line_start && (buffer.is_empty() || buffer.ends_with('\n')) {
            escape(trimmed)
        } else {
            // Line-start rules apply only where a line starts.
            let mut escaped = String::with_capacity(trimmed.len());
            for c in trimmed.chars() {
                if matches!(c, '\\' | '*' | '`' | '[' | ']' | '_') {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            escaped
        };
        push_inline(buffer, &rendered);
    }

    fn start(&mut self, name: String, attributes: &[(String, String)], self_closing: bool) {
        let void = VOID_ELEMENTS.contains(&name.as_str());
        if self.skipping > 0 {
            if !void && !self_closing {
                self.push(Open {
                    name,
                    frame: Frame::Plain,
                });
            }
            return;
        }
        if is_removed(&name, attributes) {
            if !void && !self_closing {
                self.skipping += 1;
                self.push(Open {
                    name,
                    frame: Frame::Skip,
                });
            }
            return;
        }
        self.imply_end_tags(&name);
        match name.as_str() {
            "br" => {
                if self.in_pre().is_some() {
                    self.top().push('\n');
                } else {
                    let buffer = self.top();
                    let kept = buffer.trim_end_matches(' ').len();
                    buffer.truncate(kept);
                    buffer.push_str("  \n");
                }
                return;
            }
            "hr" => {
                let buffer = self.top();
                block_break(buffer);
                buffer.push_str("* * *");
                block_break(buffer);
                return;
            }
            "img" => {
                if let Some(src) = attribute(attributes, "src").filter(|src| !src.is_empty()) {
                    let alt = attribute(attributes, "alt").unwrap_or_default();
                    let title = attribute(attributes, "title")
                        .filter(|title| !title.is_empty())
                        .map(|title| format!(" \"{}\"", title.replace('"', "\\\"")))
                        .unwrap_or_default();
                    let markdown = format!("![{}]({src}{title})", clean_attribute(alt));
                    push_inline(self.top(), &markdown);
                }
                return;
            }
            _ if void || self_closing => return,
            _ => {}
        }
        let frame = match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = usize::from(name.as_bytes()[1] - b'0');
                Frame::Buffered(Buffered::Heading(level))
            }
            "a" => match attribute(attributes, "href").filter(|href| !href.is_empty()) {
                Some(href) => Frame::Buffered(Buffered::Link {
                    href: Some(href.to_owned()),
                    title: attribute(attributes, "title")
                        .filter(|title| !title.is_empty())
                        .map(clean_attribute),
                }),
                None => Frame::Buffered(Buffered::Link {
                    href: None,
                    title: None,
                }),
            },
            "strong" | "b" => Frame::Buffered(Buffered::Delimited("**")),
            "em" | "i" => Frame::Buffered(Buffered::Delimited("_")),
            "s" | "del" | "strike" => Frame::Buffered(Buffered::Delimited("~~")),
            // turndown fences a `pre` whose first child is the `code`.
            "code"
                if self.top().is_empty()
                    && matches!(
                        self.open.last(),
                        Some(Open {
                            frame: Frame::Pre { fenced: false, .. },
                            ..
                        })
                    ) =>
            {
                let language = attribute(attributes, "class").and_then(|class| {
                    class
                        .split_whitespace()
                        .find_map(|name| name.strip_prefix("language-"))
                        .map(str::to_owned)
                });
                if let Some(Open {
                    frame:
                        Frame::Pre {
                            language: slot,
                            fenced,
                        },
                    ..
                }) = self.open.last_mut()
                {
                    *fenced = true;
                    *slot = language;
                }
                Frame::Plain
            }
            "code" => Frame::Buffered(Buffered::Code),
            "pre" => {
                block_break(self.top());
                self.buffers.push(String::new());
                Frame::Pre {
                    language: None,
                    fenced: false,
                }
            }
            "blockquote" => Frame::Buffered(Buffered::Blockquote),
            "ul" | "ol" => Frame::Buffered(Buffered::List {
                ordered: name == "ol",
                next: attribute(attributes, "start")
                    .and_then(|start| start.trim().parse::<i64>().ok())
                    .unwrap_or(1),
            }),
            "li" => Frame::Buffered(Buffered::Item),
            "table" => {
                block_break(self.top());
                Frame::Table(Table::default())
            }
            "tr" => {
                let in_head = self.find_open(&["thead"], &["table"]).is_some();
                if let Some(table) = self.innermost_table() {
                    table.rows.push(Row {
                        cells: Vec::new(),
                        in_head,
                    });
                }
                Frame::Plain
            }
            "td" | "th" => {
                if self.find_open(&["tr"], &["table"]).is_none()
                    && let Some(table) = self.innermost_table()
                {
                    table.rows.push(Row::default());
                }
                Frame::Buffered(Buffered::Cell {
                    heading: name == "th",
                    alignment: alignment(attributes),
                })
            }
            name if BLOCKS.contains(&name) => {
                block_break(self.top());
                Frame::Block
            }
            _ => Frame::Plain,
        };
        if let Frame::Buffered(_) = frame {
            if matches!(
                frame,
                Frame::Buffered(Buffered::Heading(_) | Buffered::Blockquote)
            ) {
                block_break(self.top());
            }
            self.buffers.push(String::new());
        }
        self.push(Open { name, frame });
    }

    fn innermost_table(&mut self) -> Option<&mut Table> {
        self.open
            .iter_mut()
            .rev()
            .find_map(|open| match &mut open.frame {
                Frame::Table(table) => Some(table),
                _ => None,
            })
    }

    /// The end tags HTML implies when `name` starts.
    fn imply_end_tags(&mut self, name: &str) {
        let scope = ["table", "td", "th", "caption", "button", "template", "html"];
        if CLOSES_P.contains(&name)
            && let Some(index) = self.find_open(&["p"], &scope)
        {
            self.close_to(index);
        }
        let close = |this: &mut Self, names: &[&str], boundary: &[&str]| {
            if let Some(index) = this.find_open(names, boundary) {
                this.close_to(index);
            }
        };
        match name {
            "li" => close(self, &["li"], &["ul", "ol", "table"]),
            "dt" | "dd" => close(self, &["dt", "dd"], &["dl", "table"]),
            "tr" => close(self, &["tr"], &["table"]),
            "td" | "th" => close(self, &["td", "th"], &["tr", "table"]),
            "thead" | "tbody" | "tfoot" => {
                close(self, &["thead", "tbody", "tfoot", "tr"], &["table"]);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                if self.open.last().is_some_and(|open| {
                    matches!(open.frame, Frame::Buffered(Buffered::Heading(_)))
                }) =>
            {
                self.close_top();
            }
            _ => {}
        }
    }

    fn end(&mut self, name: &str) {
        if name == "br" && self.skipping == 0 {
            self.start("br".into(), &[], true);
            return;
        }
        if self.count(name) == 0 {
            return;
        }
        if let Some(index) = self.open.iter().rposition(|open| open.name == name) {
            self.close_to(index);
        }
    }

    fn close_top(&mut self) {
        let Some(open) = self.pop() else {
            return;
        };
        match open.frame {
            Frame::Plain => {}
            Frame::Block => block_break(self.top()),
            Frame::Skip => self.skipping = self.skipping.saturating_sub(1),
            Frame::Pre { language, fenced } => {
                let code = self.buffers.pop().unwrap_or_default();
                let buffer = self.top();
                block_break(buffer);
                if fenced {
                    let longest = code
                        .lines()
                        .map(|line| line.bytes().take_while(|byte| *byte == b'`').count())
                        .filter(|run| *run >= 3)
                        .max()
                        .unwrap_or(0);
                    let fence = "`".repeat(longest.max(2) + 1);
                    let code = code.strip_suffix('\n').unwrap_or(&code);
                    let language = language.unwrap_or_default();
                    buffer.push_str(&format!("{fence}{language}\n{code}\n{fence}"));
                } else {
                    buffer.push_str(&code);
                }
                block_break(buffer);
            }
            Frame::Table(table) => {
                let rendered = table.render();
                let buffer = self.top();
                block_break(buffer);
                buffer.push_str(rendered.trim_start());
                block_break(buffer);
            }
            Frame::Buffered(kind) => {
                let content = self.buffers.pop().unwrap_or_default();
                self.finish(kind, content);
            }
        }
    }

    fn finish(&mut self, kind: Buffered, content: String) {
        match kind {
            Buffered::Heading(level) => {
                let buffer = self.top();
                block_break(buffer);
                let text = content.trim();
                if !text.is_empty() {
                    buffer.push_str(&format!("{} {text}", "#".repeat(level)));
                }
                block_break(buffer);
            }
            Buffered::Link { href, title } => {
                let (lead, text, trail) = flanking(&content);
                let markdown = match href {
                    Some(href) => {
                        let href = href.replace('(', "\\(").replace(')', "\\)");
                        let title = title
                            .map(|title| format!(" \"{}\"", title.replace('"', "\\\"")))
                            .unwrap_or_default();
                        format!("{lead}[{text}]({href}{title}){trail}")
                    }
                    None => content.clone(),
                };
                push_inline(self.top(), &markdown);
            }
            Buffered::Delimited(delimiter) => {
                let (lead, text, trail) = flanking(&content);
                if text.is_empty() {
                    push_inline(self.top(), lead);
                } else {
                    push_inline(
                        self.top(),
                        &format!("{lead}{delimiter}{text}{delimiter}{trail}"),
                    );
                }
            }
            Buffered::Code => {
                if content.is_empty() {
                    return;
                }
                let content = content.replace(['\r', '\n'], " ");
                let mut delimiter = "`".to_owned();
                let runs: Vec<usize> = content
                    .split(|c| c != '`')
                    .map(str::len)
                    .filter(|run| *run > 0)
                    .collect();
                while runs.contains(&delimiter.len()) {
                    delimiter.push('`');
                }
                let padded = content.starts_with('`')
                    || content.ends_with('`')
                    || (content.starts_with(' ')
                        && content.ends_with(' ')
                        && content.trim().len() < content.len() - 1
                        && !content.trim().is_empty());
                let space = if padded { " " } else { "" };
                push_inline(
                    self.top(),
                    &format!("{delimiter}{space}{content}{space}{delimiter}"),
                );
            }
            Buffered::Blockquote => {
                let text = content.trim_matches('\n');
                let quoted: Vec<String> =
                    text.split('\n').map(|line| format!("> {line}")).collect();
                let buffer = self.top();
                block_break(buffer);
                if !text.is_empty() {
                    buffer.push_str(&quoted.join("\n"));
                }
                block_break(buffer);
            }
            Buffered::List { .. } => {
                let parent_is_item = self
                    .open
                    .last()
                    .is_some_and(|open| matches!(open.frame, Frame::Buffered(Buffered::Item)));
                let text = content.trim_end_matches('\n');
                let buffer = self.top();
                if parent_is_item {
                    buffer.push('\n');
                    buffer.push_str(text);
                } else {
                    block_break(buffer);
                    buffer.push_str(text);
                    block_break(buffer);
                }
            }
            Buffered::Item => {
                let marker =
                    match self
                        .open
                        .iter_mut()
                        .rev()
                        .find_map(|open| match &mut open.frame {
                            Frame::Buffered(Buffered::List { ordered, next }) => {
                                Some((*ordered, next))
                            }
                            _ => None,
                        }) {
                        Some((true, next)) => {
                            let marker = format!("{next}.  ");
                            *next = next.saturating_add(1);
                            marker
                        }
                        _ => "-   ".to_owned(),
                    };
                let trimmed = content.trim_start_matches('\n');
                let body = trimmed.trim_end_matches('\n');
                let mut text = if body.len() < trimmed.len() {
                    format!("{body}\n")
                } else {
                    body.to_owned()
                };
                text = text.replace('\n', "\n    ");
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                let buffer = self.top();
                if !buffer.is_empty() && !buffer.ends_with('\n') {
                    buffer.push('\n');
                }
                buffer.push_str(&marker);
                buffer.push_str(&text);
            }
            Buffered::Cell { heading, alignment } => {
                if let Some(table) = self.innermost_table() {
                    if table.rows.is_empty() {
                        table.rows.push(Row::default());
                    }
                    if let Some(row) = table.rows.last_mut() {
                        row.cells.push(Cell {
                            text: content,
                            heading,
                            alignment,
                        });
                    }
                } else {
                    push_inline(self.top(), &content);
                }
            }
        }
    }

    fn finish_document(mut self) -> String {
        self.close_to(0);
        let document = self.buffers.into_iter().next().unwrap_or_default();
        document
            .trim_start_matches(['\t', '\r', '\n'])
            .trim_end()
            .to_owned()
    }
}

/// turndown's `cleanAttribute`: line breaks and the spaces after them
/// collapsed to one line break.
fn clean_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            while chars.peek().is_some_and(|next| next.is_whitespace()) {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// An inline element's leading space, trimmed content, and trailing
/// space: turndown moves flanking whitespace outside the delimiters.
fn flanking(content: &str) -> (&'static str, &str, &'static str) {
    let trimmed = content.trim_matches(' ');
    let lead = if trimmed.len() < content.len() && content.starts_with(' ') {
        " "
    } else {
        ""
    };
    let trail = if trimmed.len() < content.len() && content.ends_with(' ') {
        " "
    } else {
        ""
    };
    (lead, trimmed, trail)
}

/// `html` as Markdown.
pub fn html_to_markdown(html: &str) -> String {
    let mut converter = Converter::new();
    for token in tokenize(html) {
        match token {
            Token::Text(text) => converter.text(&text),
            Token::Start {
                name,
                attributes,
                self_closing,
            } => converter.start(name, &attributes, self_closing),
            Token::End(name) => converter.end(&name),
        }
    }
    converter.finish_document()
}

#[cfg(test)]
#[path = "html_tests.rs"]
mod tests;
