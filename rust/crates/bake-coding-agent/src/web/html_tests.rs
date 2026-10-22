//! The converter against Bake 0.3's: each expected string is what 0.3's
//! `formatFetchOutput` (`packages/web/tool-web/src/fetch.ts`, turndown and
//! `@joplin/turndown-plugin-gfm`) returned for the same document, after
//! the result header. Then the ports of `tool-web.spec.ts`' "fetch
//! formatting" cases for the depth preflight.

use super::*;

/// (document, 0.3's Markdown).
const SAME_AS_TYPESCRIPT: &[(&str, &str)] = &[
    (
        "<h1>Hi</h1><p>Hello <b>world</b> <a href=\"https://x.test/a\">link</a></p><ul><li>one</li><li>two<ul><li>nested</li></ul></li></ul><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2|3</td></tr></table><pre><code class=\"language-rs\">fn main() {}\n</code></pre>",
        "# Hi\n\nHello **world** [link](https://x.test/a)\n\n-   one\n-   two\n    -   nested\n\n| A   | B   |\n| --- | --- |\n| 1   | 2\\|3 |\n\n```rs\nfn main() {}\n```",
    ),
    (
        "<!doctype html><html><head><title>T</title><style>p{}</style></head><body><p>Body</p></body></html>",
        "T\n\nBody",
    ),
    (
        "<p>- item</p><p>1. foo</p><p># h</p><p>a_b*c [x] `y` \\ z</p>",
        "\\- item\n\n1\\. foo\n\n\\# h\n\na\\_b\\*c \\[x\\] \\`y\\` \\\\ z",
    ),
    (
        "<p>one<br>two</p><hr><blockquote><p>q1</p><p>q2</p></blockquote>",
        "one  \ntwo\n\n* * *\n\n> q1\n> \n> q2",
    ),
    ("<ol start=\"3\"><li>a</li><li>b</li></ol>", "3.  a\n4.  b"),
    (
        "<table><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></table>",
        "|     |     |\n| --- | --- |\n| 1   | 2   |\n| 3   | 4   |",
    ),
    (
        "<p><strong> bold </strong>and<em>em</em> <code>a`b</code> <s>del</s></p>",
        "**bold** and_em_ ``a`b`` ~~del~~",
    ),
    (
        "<a href=\"/x(1)\" title='T \"q\"'>link</a> <a>noref</a> <img src=\"i.png\" alt=\"Alt\">",
        "[link](/x\\(1\\) \"T \\\"q\\\"\") noref ![Alt](i.png)",
    ),
    (
        "<div hidden>h</div><span aria-hidden=\"true\">a</span><p style=\"display: none !important\">d</p><input type=hidden value=v><script>x</script><noscript>n</noscript>visible",
        "visible",
    ),
    (
        "<pre>\n  keep   spaces\n\n\nx</pre>",
        "  keep   spaces\n\n\nx",
    ),
    (
        "<p>&amp; &lt;tag&gt; &copy; &#x41;&#66; x &unknown;</p>",
        "& <tag> © AB x &unknown;",
    ),
    (
        "<h2>Title <a href=\"https://a.b\">link</a></h2>text after",
        "## Title [link](https://a.b)\n\ntext after",
    ),
    (
        "<ul><li><p>para one</p><p>para two</p></li><li>x</li></ul>",
        "-   para one\n    \n    para two\n    \n-   x",
    ),
    (
        "<table><thead><tr><th align=\"right\">A</th><th style=\"text-align:center\">B</th></tr></thead><tbody><tr><td>x</td><td>y\nz</td></tr></tbody></table>",
        "| A   | B   |\n| ---: | :---: |\n| x   | y z |",
    ),
    ("<table><tr><td>only</td></tr></table>", "| only |"),
    (
        "<p>Unclosed para<p>Second<div>Block</div>",
        "Unclosed para\n\nSecond\n\nBlock",
    ),
    ("<ul><li>a<li>b</ul>", "-   a\n-   b"),
    (
        "<p>text <code>x_y*z</code> and <kbd>K</kbd></p>",
        "text `x_y*z` and K",
    ),
    (
        "<pre><code>```\ninner\n```</code></pre>",
        "````\n```\ninner\n```\n````",
    ),
    (
        "<div><span>inline</span> <span>words</span></div><div>next</div>",
        "inline words\n\nnext",
    ),
    (
        "<p style=\"visibility:hidden\">x</p><template><p>t</p></template><iframe src=\"x\">i</iframe>shown",
        "shown",
    ),
    (
        "<table><tr><td colspan=\"1000000\">wide</td><td>b</td></tr></table>",
        "|     |     |\n| --- | --- |\n| wide | b   |",
    ),
    ("<h3>  spaced   heading  </h3>", "### spaced heading"),
    (
        "<blockquote>quote<br>line</blockquote>",
        "> quote  \n> line",
    ),
    (
        "<p>a</p>\n\n<p>b</p>\n<ul>\n  <li>x</li>\n  <li>y</li>\n</ul>",
        "a\n\nb\n\n-   x\n-   y",
    ),
];

#[test]
fn converts_as_the_typescript_converter_did() {
    for (html, expected) in SAME_AS_TYPESCRIPT {
        assert_eq!(html_to_markdown(html), *expected, "{html}");
    }
}

// tool-web.spec.ts: "converts html via turndown and drops active or
// hidden content".
#[test]
fn drops_active_or_hidden_content() {
    assert_eq!(
        html_to_markdown(
            r#"<style>.x{}</style><script>bad()</script><noscript>ns</noscript><template>template</template><iframe>frame</iframe><object>object</object><embed src="hidden"><p hidden>hidden</p><p aria-hidden="true">aria</p><p style="display: none !important">display</p><p style="visibility:collapse">visibility</p><input type="hidden" value="secret"><p style="color red">Tom &amp; Jerry &copy; R&eacute;sum&eacute;</p><a href="https://a.test">link</a>"#
        ),
        "Tom & Jerry © Résumé\n\n[link](https://a.test)"
    );
    assert_eq!(
        html_to_markdown("<h2>Heading</h2><ul><li>one</li><li>two</li></ul>"),
        "## Heading\n\n-   one\n-   two"
    );
    assert_eq!(
        html_to_markdown(
            r#"<table><thead><tr><th align="left">L</th><th align="right">R</th><th style="text-align:center">C</th></tr></thead><tbody><tr><td>1</td><td>2</td><td>3</td></tr></tbody></table>"#
        ),
        "| L   | R   | C   |\n| :--- | ---: | :---: |\n| 1   | 2   | 3   |"
    );
    assert_eq!(
        html_to_markdown(
            "<p><strong>bold <em>italic</em></strong></p><blockquote><p>quoted</p></blockquote>"
        ),
        "**bold _italic_**\n\n> quoted"
    );
}

// "does not expand numeric colspan attributes into unbounded output".
#[test]
fn does_not_expand_colspan() {
    assert_eq!(
        html_to_markdown(
            r#"<table><thead><tr><th colspan="1000000">A</th></tr></thead><tbody><tr><td>B</td></tr></tbody></table>"#
        ),
        "| A   |\n| --- |\n| B   |"
    );
}

// "omits deeply nested html without attempting conversion": the
// preflight flags it, and the converter's own stack is a vector, so even
// unflagged nesting cannot exhaust the call stack.
#[test]
fn flags_deep_nesting_and_converts_it_without_recursion() {
    let depth = 20_000;
    let pathological = format!("{}x{}", "<div>".repeat(depth), "</div>".repeat(depth));
    assert!(exceeds_conversion_depth(&pathological));
    assert_eq!(html_to_markdown(&pathological), "x");
}

// "comments and mismatched closing tags cannot hide deep nesting from the
// preflight".
#[test]
fn comments_and_mismatched_closing_tags_cannot_hide_nesting() {
    assert!(exceeds_conversion_depth(&format!(
        "{}x",
        "<div><!-- </div> --></span>".repeat(600)
    )));
    assert!(exceeds_conversion_depth(&format!(
        "{}x",
        "<div><!-->".repeat(600)
    )));
}

// "the preflight accepts ordinary closed, void, self-closing, quoted, and
// raw-text markup".
#[test]
fn the_preflight_accepts_ordinary_markup() {
    let paragraphs = r#"<p title='>'>x<br   ><img src="x"><input/></p>"#.repeat(600);
    let script = format!(
        "<script>const invalid = '</scriptx>'; const template = '{}'</script >",
        "<div>".repeat(600)
    );
    let document = format!("<!doctype html><?pi><1bad>{paragraphs}{script}");
    assert!(!exceeds_conversion_depth(&document));
    assert!(!html_to_markdown(&document).contains("<p"));
    assert_eq!(html_to_markdown("plain text"), "plain text");
    assert_eq!(html_to_markdown("<p>x</p><!-- unfinished"), "x");
    assert_eq!(html_to_markdown("<script>unclosed"), "");
    assert_eq!(html_to_markdown("<script>closed by slash</script/>"), "");
    assert_eq!(html_to_markdown("<script>closed at end</script"), "");
}

// "scans malformed unterminated tags in bounded time": both passes are
// linear, so this finishes at once even unoptimized.
#[test]
fn scans_malformed_unterminated_tags_in_bounded_time() {
    let started = std::time::Instant::now();
    let malformed = "<a".repeat(100_000);
    assert!(!exceeds_conversion_depth(&malformed));
    let _ = html_to_markdown(&malformed);
    let comment = format!("<!--{}", "<b".repeat(100_000));
    assert!(!exceeds_conversion_depth(&comment));
    assert_eq!(html_to_markdown(&comment), "");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

// turndown's `fencedCodeBlock` takes only a `pre` whose first child is a
// `code`; another `pre` stays an escaped block with its whitespace, its
// `code` inline code. The expected strings are 0.3's output.
#[test]
fn fences_only_a_pre_that_opens_with_code() {
    assert_eq!(
        html_to_markdown("<pre>\n<code class=\"language-sh\">ls *</code></pre>"),
        "```sh\nls *\n```"
    );
    assert_eq!(
        html_to_markdown("<pre>$ <code>ls *</code></pre>"),
        "$ `ls *`"
    );
    assert_eq!(html_to_markdown("<pre>a *b*</pre>"), "a \\*b\\*");
}

#[test]
fn decodes_references() {
    assert_eq!(
        decode_entities("&amp;&#x1F600;&#0;&#xD800;&nosuch; &lt &copy"),
        "&😀\u{fffd}\u{fffd}&nosuch; < &copy"
    );
}
