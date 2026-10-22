//! The codemode source format, ported from Pi's
//! `packages/codemode/src/source.ts` (v1.3.0; MIT, see `NOTICE`): JavaScript,
//! optionally preceded by one options line.
//!
//! ```js
//! // @options: {"max_output_tokens": 2000, "timeout_ms": 30000}
//! const text = await tools.read({ path: "package.json" });
//! text(JSON.parse(text).name);
//! ```
//!
//! Pi's Lark grammar for grammar-constrained input is not ported (D42).

use std::fmt;

use serde_json::Value;

/// The prefix of the options line.
pub const CODEMODE_OPTIONS_PREFIX: &str = "// @options:";

const SUPPORTED_FIELDS: [&str; 2] = ["max_output_tokens", "timeout_ms"];
const SUPPORTED_FIELDS_TEXT: &str = "`max_output_tokens` and `timeout_ms`";
/// Largest delay JavaScript's `setTimeout` supports, which bounds `timeout_ms`.
pub const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The options a script's first line sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceOptions {
    /// Token budget for the script's output.
    pub max_output_tokens: Option<u64>,
    /// Hard deadline for the whole script in milliseconds, including tool calls.
    pub timeout_ms: Option<u64>,
}

/// A parsed script: the code the VM runs and its options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    /// The script with the options line replaced by an empty line, so line
    /// numbers are unchanged.
    pub code: String,
    /// What the options line set.
    pub options: SourceOptions,
}

impl Source {
    /// Code that runs as written, with no options; what Pi's `execute()` takes.
    pub fn plain(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            options: SourceOptions::default(),
        }
    }

    /// Splits an optional first-line `// @options: {...}` from the script.
    ///
    /// # Errors
    ///
    /// [`SourceError`] for empty input, invalid options, and an options line
    /// that is not the first line, which would otherwise be ignored without
    /// notice.
    pub fn parse(input: &str) -> Result<Self, SourceError> {
        if trim_js(input).is_empty() {
            return Err(SourceError(
                "Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`.".to_owned(),
            ));
        }
        let lines: Vec<&str> = input.split('\n').collect();
        if let Some(misplaced) = lines
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, line)| trim_start_js(line).starts_with(CODEMODE_OPTIONS_PREFIX))
            .map(|(index, _)| index)
        {
            return Err(SourceError(format!(
                "The {CODEMODE_OPTIONS_PREFIX} line must be the first line of the input; found one on line {}",
                misplaced + 1
            )));
        }
        let first = lines.first().copied().unwrap_or("");
        let first = first.strip_suffix('\r').unwrap_or(first);
        let trimmed = trim_start_js(first);
        let Some(directive) = trimmed.strip_prefix(CODEMODE_OPTIONS_PREFIX) else {
            return Ok(Self::plain(input));
        };
        let code = input.find('\n').map_or("", |newline| &input[newline..]);
        if trim_js(code).is_empty() {
            return Err(SourceError(
                "The @options line must be followed by JavaScript source on subsequent lines"
                    .to_owned(),
            ));
        }
        Ok(Self {
            code: code.to_owned(),
            options: parse_options(trim_js(directive))?,
        })
    }
}

/// Why a script's source was refused; the message is Pi's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError(pub String);

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SourceError {}

/// JavaScript's `trim()` whitespace: Unicode white space and the BOM.
fn is_js_space(character: char) -> bool {
    character.is_whitespace() || character == '\u{feff}'
}

fn trim_js(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

fn trim_start_js(text: &str) -> &str {
    text.trim_start_matches(is_js_space)
}

/// A non-negative safe integer, as `Number.isSafeInteger(value) && value >= 0`.
fn safe_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    (number.is_finite() && number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number))
        .then_some(number as u64)
}

fn parse_options(directive: &str) -> Result<SourceOptions, SourceError> {
    let object_error = || {
        SourceError(format!(
            "@options must be a JSON object with supported fields {SUPPORTED_FIELDS_TEXT}"
        ))
    };
    if directive.is_empty() {
        return Err(object_error());
    }
    let value: Value = serde_json::from_str(directive).map_err(|error| {
        SourceError(format!(
            "@options must be valid JSON with supported fields {SUPPORTED_FIELDS_TEXT}: {error}"
        ))
    })?;
    let Value::Object(fields) = value else {
        return Err(object_error());
    };
    if let Some(key) = fields
        .keys()
        .find(|key| !SUPPORTED_FIELDS.contains(&key.as_str()))
    {
        return Err(SourceError(format!(
            "@options only supports {SUPPORTED_FIELDS_TEXT}; got `{key}`"
        )));
    }
    let mut options = SourceOptions::default();
    if let Some(value) = fields.get("max_output_tokens") {
        options.max_output_tokens = Some(safe_integer(value).ok_or_else(|| {
            SourceError(
                "@options field `max_output_tokens` must be a non-negative safe integer".to_owned(),
            )
        })?);
    }
    if let Some(value) = fields.get("timeout_ms") {
        match safe_integer(value) {
            Some(timeout) if timeout > 0 && timeout <= MAX_TIMEOUT_MS => {
                options.timeout_ms = Some(timeout)
            }
            _ => {
                return Err(SourceError(format!(
                    "@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}"
                )));
            }
        }
    }
    Ok(options)
}

/// Pi's `test/source.test.ts`, case for case.
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Source {
        match Source::parse(input) {
            Ok(source) => source,
            Err(error) => panic!("{input:?} was refused: {error}"),
        }
    }

    fn refusal(input: &str) -> String {
        match Source::parse(input) {
            Ok(source) => panic!("{input:?} was accepted as {source:?}"),
            Err(error) => error.0,
        }
    }

    #[test]
    fn returns_plain_code_unchanged() {
        assert_eq!(parse("text('hi')"), Source::plain("text('hi')"));
        assert_eq!(
            parse("// just a comment\nreturn 1"),
            Source::plain("// just a comment\nreturn 1")
        );
    }

    #[test]
    fn parses_the_options_line_and_keeps_line_numbers() {
        assert_eq!(
            parse("// @options: {\"timeout_ms\": 10}\nconst a = 1;\ntext(a)"),
            Source {
                code: "\nconst a = 1;\ntext(a)".to_owned(),
                options: SourceOptions {
                    max_output_tokens: None,
                    timeout_ms: Some(10)
                },
            }
        );
        assert_eq!(
            parse("  // @options:{\"max_output_tokens\":0,\"timeout_ms\":1500}\r\ntext(1)").options,
            SourceOptions {
                max_output_tokens: Some(0),
                timeout_ms: Some(1500)
            }
        );
        assert_eq!(
            parse("// @options: {}\ntext(1)"),
            Source {
                code: "\ntext(1)".to_owned(),
                options: SourceOptions::default()
            }
        );
    }

    #[test]
    fn only_treats_the_first_line_as_an_options_line() {
        assert_eq!(
            parse("// @optionsx {}\ntext(1)").options,
            SourceOptions::default()
        );
        assert_eq!(
            parse("text(\"// @options: {}\")").options,
            SourceOptions::default()
        );
    }

    // #10631: an options line after the first line used to be ignored, so its timeout never applied.
    #[test]
    fn rejects_an_options_line_that_is_not_the_first_line() {
        let cases = [
            (
                "// count things\n// @options: {\"timeout_ms\": 1000}\nwhile (true) {}",
                2,
            ),
            (
                "\n  // @options: {\"timeout_ms\": 1000}\nwhile (true) {}",
                2,
            ),
            ("text(1)\r\n// @options: {\"timeout_ms\": 1}", 2),
            (
                "// @options: {\"timeout_ms\": 1}\ntext(1)\n// @options: {\"timeout_ms\": 2}",
                3,
            ),
        ];
        for (input, line) in cases {
            assert_eq!(
                refusal(input),
                format!(
                    "The // @options: line must be the first line of the input; found one on line {line}"
                ),
                "{input:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_input_and_invalid_options() {
        let cases = [
            ("", "Expected JavaScript source text (non-empty)"),
            ("  \n", "Expected JavaScript source text (non-empty)"),
            (
                "// @options:\ntext(1)",
                "@options must be a JSON object with supported fields",
            ),
            (
                "// @options: {timeout_ms: 1}\ntext(1)",
                "@options must be valid JSON with supported fields",
            ),
            (
                "// @options: [1]\ntext(1)",
                "@options must be a JSON object with supported fields",
            ),
            (
                "// @options: {\"yield\": 1}\ntext(1)",
                "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`",
            ),
            (
                "// @options: {\"max_output_tokens\": 1.5}\ntext(1)",
                "@options field `max_output_tokens` must be a non-negative safe integer",
            ),
            (
                "// @options: {\"timeout_ms\": 0}\ntext(1)",
                "@options field `timeout_ms` must be a positive integer",
            ),
            (
                "// @options: {\"timeout_ms\": 1}",
                "The @options line must be followed by JavaScript source on subsequent lines",
            ),
            (
                "// @options: {\"timeout_ms\": 1}\n  \n",
                "The @options line must be followed by JavaScript source on subsequent lines",
            ),
        ];
        for (input, message) in cases {
            let refused = refusal(input);
            assert!(refused.contains(message), "{input:?}: {refused}");
        }
    }
}
