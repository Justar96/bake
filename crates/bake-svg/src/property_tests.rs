//! Invariants over untrusted sources: nothing panics, every repair is a
//! prefix of the source with only close tags after it, and a repair that
//! parses draws within the size asked for.

use proptest::prelude::*;

use super::*;

/// Fragments that make SVG-like markup, valid or not.
fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("<svg viewBox=\"0 0 8 8\">".to_owned()),
        Just(
            "<svg:svg xmlns:svg=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\">"
                .to_owned()
        ),
        Just("<g fill=\"red\">".to_owned()),
        Just("</g>".to_owned()),
        Just("</svg>".to_owned()),
        Just("<rect width=\"3\" height=\"2\"/>".to_owned()),
        Just("<circle cx=\"4\" cy=\"4\" r=\"3\" stroke=\"#0af\"/>".to_owned()),
        Just("<path d=\"M0 0 L8 8\"/>".to_owned()),
        Just("<text x=\"1\" y=\"6\">a&amp;b</text>".to_owned()),
        Just("<!-- <g> -->".to_owned()),
        Just("<![CDATA[ <x> ]]>".to_owned()),
        Just("<use href=\"#a\"/>".to_owned()),
        Just("<g id=\"a\">".to_owned()),
        Just("<marker id=\"m\">".to_owned()),
        Just("</marker>".to_owned()),
        Just("<polyline points=\"0,0 4,4 8,0\" marker-mid=\"url(#m)\"/>".to_owned()),
        Just("<style>rect{fill:blue}</style>".to_owned()),
        Just("\"'>".to_owned()),
        "[<>/=\"' a-z0-9#&;:!?\\[\\]-]{0,12}",
    ]
}

fn source() -> impl Strategy<Value = String> {
    prop::collection::vec(fragment(), 0..24).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn repairs_are_prefixes_closed_by_end_tags(source in source(), cut in 0usize..400) {
        let mut end = cut.min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let partial = &source[..end];
        let repaired = repairs(partial);
        prop_assert!(repaired.len() <= REPAIRS);
        for document in &repaired {
            // Taking close tags off its end reaches a prefix of the source.
            let mut rest = document.as_str();
            while !partial.starts_with(rest) {
                let open = rest.rfind("</");
                prop_assert!(rest.ends_with('>') && open.is_some(), "{document:?}");
                let open = open.unwrap_or_default();
                prop_assert!(!rest[open + 2..rest.len() - 1].contains(['<', '>', '"', '\'', ' ']));
                rest = &rest[..open];
            }
        }
    }

    #[test]
    fn parsing_and_drawing_never_panic(source in source(), width in 0u32..64, height in 0u32..64) {
        let fonts = Fonts::none();
        let _ = first_element(&source);
        for document in std::iter::once(source.clone()).chain(repairs(&source)) {
            if let Ok(svg) = parse(&document, &fonts) {
                prop_assert!(svg.outline().nodes <= MAX_NODES);
                let report = svg.report();
                prop_assert!(report.hidden_shapes <= report.shapes);
                let raster = svg.render(width, height).unwrap();
                prop_assert_eq!(
                    raster.rgba.len(),
                    raster.width as usize * raster.height as usize * 4
                );
                prop_assert!(raster.width <= width.max(1) && raster.height <= height.max(1));
            }
        }
    }

    #[test]
    fn fit_stays_inside_its_box(
        w in 0.0f32..10_000.0,
        h in 0.0f32..10_000.0,
        mw in 0u32..500,
        mh in 0u32..500,
        aspect in 0.1f32..4.0,
    ) {
        let (fw, fh) = fit(w, h, mw, mh, aspect);
        prop_assert!(fw >= 1 && fh >= 1);
        prop_assert!(fw <= mw.max(1) && fh <= mh.max(1));
    }
}
