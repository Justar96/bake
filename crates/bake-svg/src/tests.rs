use super::*;

const SQUARE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#ff0000"/></svg>"##;

fn parsed(source: &str) -> Svg {
    parse(source, &Fonts::none()).expect("parses")
}

#[test]
fn a_square_draws_its_colour_at_every_pixel() {
    let svg = parsed(SQUARE);
    assert_eq!(
        svg.outline(),
        Outline {
            width: 10.0,
            height: 10.0,
            elements: 1,
            nodes: 1,
            texts: 0
        }
    );
    let raster = svg.render(4, 4).unwrap();
    assert_eq!((raster.width, raster.height), (4, 4));
    for y in 0..4 {
        for x in 0..4 {
            assert_eq!(raster.pixel(x, y), [255, 0, 0, 255]);
        }
    }
    assert_eq!(raster.pixel(4, 0), [0; 4]);
}

#[test]
fn half_transparent_pixels_keep_their_straight_colour() {
    let svg = parsed(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 1"><rect width="1" height="1" fill="#0000ff" fill-opacity="0.5"/></svg>"##,
    );
    let raster = svg.render(2, 1).unwrap();
    let [r, g, b, a] = raster.pixel(0, 0);
    assert_eq!((r, g), (0, 0));
    assert!(b >= 250, "{b}");
    assert!((126..=129).contains(&a), "{a}");
    assert_eq!(raster.pixel(1, 0), [0; 4]);
}

#[test]
fn what_could_read_outside_or_grow_is_refused() {
    let fonts = Fonts::none();
    let big = format!("<svg>{}</svg>", " ".repeat(MAX_SOURCE_BYTES));
    assert_eq!(
        parse(&big, &fonts).unwrap_err(),
        Error::TooLarge {
            bytes: MAX_SOURCE_BYTES + 11
        }
    );
    let laughs =
        r#"<!DOCTYPE svg [<!ENTITY a "aaaa">]><svg viewBox="0 0 1 1"><text>&a;</text></svg>"#;
    assert_eq!(parse(laughs, &fonts).unwrap_err(), Error::Entities);
    assert_eq!(
        parse("<html><svg/></html>", &fonts).unwrap_err(),
        Error::NotSvg
    );
    assert_eq!(
        parse(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="5"/>"#,
            &fonts
        )
        .unwrap_err(),
        Error::NoSize
    );
    let Error::Malformed { line, .. } =
        parse("<svg viewBox=\"0 0 1 1\">\n<rect></svg>", &fonts).unwrap_err()
    else {
        panic!("malformed");
    };
    assert_eq!(line, 2);
}

#[test]
fn linked_images_draw_nothing() {
    for href in [
        "/etc/hosts",
        "file:///etc/hosts",
        "https://example.com/a.png",
        // <svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 1 1'><rect width='1' height='1'/></svg>
        "data:image/svg+xml;base64,PHN2ZyB4bWxucz0naHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmcnIHZpZXdCb3g9JzAgMCAxIDEnPjxyZWN0IHdpZHRoPScxJyBoZWlnaHQ9JzEnLz48L3N2Zz4=",
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
    ] {
        let source = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><image href="{href}" width="4" height="4"/></svg>"#
        );
        let svg = parsed(&source);
        assert_eq!(svg.outline().nodes, 0, "{href}");
        let raster = svg.render(4, 4).unwrap();
        assert!(raster.rgba.iter().all(|byte| *byte == 0), "{href}");
    }
}

#[test]
fn render_refuses_sizes_past_the_pixel_limit() {
    let svg = parsed(SQUARE);
    assert_eq!(svg.render(0, 5).unwrap().width, 1);
    assert_eq!(svg.render(3000, 3000).unwrap().rgba, vec![0; 4]);
}

#[test]
fn fit_keeps_proportions_for_pixels_of_any_shape() {
    // Square pixels: a 2:1 drawing in a 100 by 100 box.
    assert_eq!(fit(200.0, 100.0, 100, 100, 1.0), (100, 50));
    // Pixels twice as tall as wide need half as many rows.
    assert_eq!(fit(200.0, 100.0, 100, 100, 2.0), (100, 25));
    // A tall drawing is bound by the height.
    assert_eq!(fit(100.0, 400.0, 100, 40, 1.0), (10, 40));
    // No size, or no room, takes the box.
    assert_eq!(fit(0.0, 10.0, 7, 3, 1.0), (7, 3));
    assert_eq!(fit(10.0, 10.0, 0, 3, 1.0), (1, 3));
    assert_eq!(fit(1.0, 1000.0, 50, 4, 1.0), (1, 4));
}

#[test]
fn repairs_close_what_is_open_at_the_last_complete_tags() {
    let source = r#"<?xml version="1.0"?><!-- a <b> --><svg viewBox="0 0 4 4"><g fill="red"><rect width="2" height="2"/><rect wid"#;
    assert_eq!(
        repairs(source),
        [
            r#"<?xml version="1.0"?><!-- a <b> --><svg viewBox="0 0 4 4"><g fill="red"><rect width="2" height="2"/></g></svg>"#,
            r#"<?xml version="1.0"?><!-- a <b> --><svg viewBox="0 0 4 4"><g fill="red"></g></svg>"#,
            r#"<?xml version="1.0"?><!-- a <b> --><svg viewBox="0 0 4 4"></svg>"#,
        ]
    );
    for repaired in repairs(source) {
        parsed(&repaired);
    }
    // A `>` inside a quoted value does not end the tag.
    assert_eq!(
        repairs(r#"<svg viewBox="0 0 1 1"><text x="a>b"#),
        [r#"<svg viewBox="0 0 1 1"></svg>"#]
    );
    // Nothing until the root's tag is complete.
    assert!(repairs("<sv").is_empty());
    assert!(repairs("<svg viewBox=\"0 0").is_empty());
    // A complete document is its own first repair.
    assert_eq!(repairs(SQUARE)[0], SQUARE);
    // Only the last few complete tags are kept.
    let many = format!("<svg viewBox=\"0 0 1 1\">{}", "<g/>".repeat(10));
    assert_eq!(repairs(&many).len(), REPAIRS);
    // A close tag that does not match stops the scan before it.
    assert_eq!(
        repairs("<svg viewBox=\"0 0 1 1\"><g></a><rect/>"),
        [
            "<svg viewBox=\"0 0 1 1\"><g></g></svg>",
            "<svg viewBox=\"0 0 1 1\"></svg>"
        ]
    );
}

#[test]
fn system_fonts_draw_text() {
    let fonts = Fonts::system();
    if fonts.faces() == 0 {
        // A host without fonts draws no text; nothing to check.
        return;
    }
    let svg = parse(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20"><text x="2" y="16" font-size="16" font-family="sans-serif">Hi</text></svg>"#,
        &fonts,
    )
    .unwrap();
    assert_eq!((svg.outline().elements, svg.outline().texts), (1, 1));
    let raster = svg.render(40, 20).unwrap();
    assert!(raster.rgba.chunks(4).any(|pixel| pixel[3] > 0));
    let blank = parse(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20"><text x="2" y="16" font-size="16">Hi</text></svg>"#,
        &Fonts::none(),
    )
    .unwrap()
    .render(40, 20)
    .unwrap();
    assert!(blank.rgba.iter().all(|byte| *byte == 0));
}

#[test]
fn a_png_holds_the_raster() {
    let png = parsed(SQUARE).render_png(3, 2).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    // The header's width and height.
    assert_eq!(&png[16..24], &[0, 0, 0, 3, 0, 0, 0, 2]);
}

// A `<use>` counts as a copy of what it links to, so nesting that would
// multiply the tree is refused from the source, and plain reuse is not.
#[test]
fn uses_are_counted_as_the_copies_they_make() {
    let fonts = Fonts::none();
    let document = |body: &str| {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">{body}</svg>"#)
    };
    // Many uses of one shape: each copies one element.
    let reused = document(&format!(
        r##"<rect id="r" width="1" height="1"/>{}"##,
        r##"<use href="#r"/>"##.repeat(500)
    ));
    assert_eq!(parse(&reused, &fonts).unwrap().outline().elements, 501);
    // Sixteen groups, each using the one before twice: 2^16 copies.
    let mut doubling = String::from(r#"<g id="g0"><rect width="1" height="1"/></g>"#);
    for level in 1..=16 {
        let before = level - 1;
        doubling.push_str(&format!(
            r##"<g id="g{level}"><use href="#g{before}"/><use xlink:href="#g{before}"/></g>"##
        ));
    }
    let doubling = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 8 8">{doubling}</svg>"#
    );
    assert_eq!(parse(&doubling, &fonts).unwrap_err(), Error::TooManyNodes);
    // A use that links to what holds it copies nothing there.
    let looped = document(r##"<g id="a"><rect width="1" height="1"/><use href="#a"/></g>"##);
    let xml = roxmltree::Document::parse(&looped).unwrap();
    assert_eq!(expanded(&xml), 4);
    assert!(parse(&looped, &fonts).is_ok());
    // Nesting far deeper than the thread's stack would take is counted.
    let deep = format!("<svg>{}{}</svg>", "<g>".repeat(5_000), "</g>".repeat(5_000));
    let xml = roxmltree::Document::parse(&deep).unwrap();
    assert_eq!(expanded(&xml), 5_001);
}

// A marker is drawn at each vertex of the shapes that take one.
#[test]
fn markers_are_counted_once_per_vertex() {
    let marker = format!(
        r#"<marker id="m">{}</marker>"#,
        r#"<rect width="1" height="1"/>"#.repeat(40)
    );
    let points = "1,1 ".repeat(1_000);
    let marked = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">{marker}<polyline points="{points}" marker-mid="url(#m)"/></svg>"##
    );
    assert_eq!(
        parse(&marked, &Fonts::none()).unwrap_err(),
        Error::TooManyNodes
    );
    // The same shapes without a marker on the line are a few elements.
    let plain = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8">{marker}<polyline points="{points}"/><line x2="4" y2="4" marker-end="url(#m)"/></svg>"##
    );
    let xml = roxmltree::Document::parse(&plain).unwrap();
    // The root, the marker and its 40, the polyline, and the line with a
    // marker at each of its 2 ends.
    assert_eq!(expanded(&xml), 1 + 41 + 1 + (1 + 2 * 41));
    assert!(parse(&plain, &Fonts::none()).is_ok());
}

#[test]
fn a_report_counts_the_shapes_outside_the_canvas() {
    let svg = parsed(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10"/><rect x="-4" width="8" height="2"/><circle cx="40" cy="5" r="3"/><g clip-path="url(#c)"><rect x="50" width="2" height="2"/></g><clipPath id="c"><rect width="5" height="5"/></clipPath></svg>"##,
    );
    let report = svg.report();
    // The clipped rect is left out; the circle is the one wholly outside.
    assert_eq!((report.shapes, report.hidden_shapes), (3, 1));
    assert!(report.texts.is_empty());
}

#[test]
fn a_report_measures_texts_as_laid_out() {
    let fonts = Fonts::system();
    if fonts.faces() == 0 {
        // A host without fonts lays out no text; nothing to check.
        return;
    }
    let svg = parse(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><g transform="scale(2)"><text x="5" y="20" font-size="6">Second
  line <tspan>here</tspan></text></g><text x="150" y="12" font-size="10">A label far too long for the canvas it sits on</text><text x="10" y="42" font-size="12">Second</text><text x="900" y="50" font-size="10">Lost</text></svg>"##,
        &fonts,
    )
    .unwrap();
    let report = svg.report();
    let labels: Vec<&str> = report
        .texts
        .iter()
        .map(|text| text.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "A label far too long for the can…",
            "Second line here",
            "Second",
            "Lost"
        ]
    );
    let [long, scaled, plain, lost] = &report.texts[..] else {
        panic!("four texts");
    };
    // The scale doubles the font size the canvas shows.
    assert!((scaled.size - 12.0).abs() < 0.01, "{}", scaled.size);
    assert!((plain.size - 12.0).abs() < 0.01, "{}", plain.size);
    // The long label passes the right edge only; the lost one is outside.
    let [left, top, right, bottom] = long.past(200.0, 100.0);
    assert!(
        left == 0.0 && top == 0.0 && bottom == 0.0 && right > 20.0,
        "{right}"
    );
    assert!(!long.outside(200.0, 100.0) && lost.outside(200.0, 100.0));
    // The two texts at the same place overlap; the long one is clear.
    assert!(scaled.overlap(plain) > 0.5, "{}", scaled.overlap(plain));
    assert_eq!(long.overlap(plain), 0.0);
}

#[test]
fn a_raster_knows_how_much_of_it_is_clear() {
    let half = parsed(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="2" height="4" fill="#fff"/></svg>"##,
    );
    assert_eq!(half.render(4, 4).unwrap().clear_share(), 0.5);
    assert_eq!(parsed(SQUARE).render(4, 4).unwrap().clear_share(), 0.0);
}
