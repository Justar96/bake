//! Guard against streamed tool calls that pad runaway whitespace after their
//! arguments without closing the JSON object.
//!
//! Ported from Bake `packages/llm/llm-pi-ai/src/stream.ts` and `config.ts`.
//! Some models finish a tool call's last argument value and then stream
//! whitespace for minutes without closing the object; every padding delta
//! resets any idle timeout, so counting trailing whitespace outside JSON
//! strings is how the stream detects the runaway and fails the request as a
//! retryable transport error.

/// Default longest run of whitespace outside JSON strings that a tool call's
/// streamed arguments may end with before the response is abandoned.
pub const DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE: usize = 2000;

/// Scan state for one streaming tool call's argument text, kept so each delta
/// is read once: whether the scan is inside a JSON string (and just after a
/// backslash there), and how many whitespace characters outside any string
/// ended the text so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArgumentScan {
    /// Inside a JSON string literal.
    pub in_string: bool,
    /// Immediately after `\` inside a JSON string literal.
    pub escaped: bool,
    /// Trailing JSON whitespace characters (` `, `\n`, `\r`, `\t`) outside
    /// any string literal.
    pub trailing_whitespace: usize,
}

impl ArgumentScan {
    /// Advances the scan over `delta`. Only JSON whitespace outside strings
    /// counts: whitespace inside a string value is content the model may
    /// legitimately write, such as a file's indentation, while a runaway pads
    /// between tokens. Any other character outside a string resets the run.
    pub fn scan_arguments(&mut self, delta: &str) {
        for code in delta.encode_utf16() {
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if code == 0x5C {
                    self.escaped = true;
                } else if code == 0x22 {
                    self.in_string = false;
                }
                continue;
            }
            if matches!(code, 0x20 | 0x0A | 0x0D | 0x09) {
                self.trailing_whitespace += 1;
                continue;
            }
            self.trailing_whitespace = 0;
            if code == 0x22 {
                self.in_string = true;
            }
        }
    }

    /// Scans `delta` and fails when `trailing_whitespace` exceeds `max`.
    pub fn check_delta(
        &mut self,
        delta: &str,
        max: usize,
        model: &str,
        tool_name: &str,
    ) -> Result<(), String> {
        self.scan_arguments(delta);
        if self.trailing_whitespace > max {
            return Err(format!(
                "model \"{model}\" streamed {} whitespace characters after the arguments of tool call \"{tool_name}\" without closing them",
                self.trailing_whitespace
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Cases ported from `packages/llm/llm-pi-ai/tests/tool-argument-guard.spec.ts`.

    use super::*;
    use crate::providers::faux::faux_assistant_message;
    use crate::types::StopReason;
    use crate::utils::retry::is_retryable_assistant_error;

    #[test]
    fn abandons_a_tool_call_whose_arguments_end_in_a_whitespace_run_past_the_bound() {
        let mut scan = ArgumentScan::default();
        assert_eq!(
            scan.check_delta(
                r#"{"file_path":"src/money.js","old_string":"a","edits":[]"#,
                100,
                "m",
                "edit"
            ),
            Ok(())
        );
        let mut forwarded = 1usize;
        let mut failure = None;
        for _ in 0..30 {
            match scan.check_delta(" \n\t\r ", 100, "m", "edit") {
                Ok(()) => forwarded += 1,
                Err(message) => {
                    failure = Some(message);
                    break;
                }
            }
        }
        let message = failure.expect("guard fired");
        assert_eq!(
            message,
            r#"model "m" streamed 105 whitespace characters after the arguments of tool call "edit" without closing them"#
        );
        // The 21st five-character delta crossed the 100-char bound and was not forwarded.
        assert_eq!(forwarded, 21);
        let mut assistant = faux_assistant_message("");
        assistant.stop_reason = StopReason::Error;
        assistant.error_message = Some(message);
        assert!(is_retryable_assistant_error(&assistant));
    }

    #[test]
    fn counts_whitespace_across_delta_boundaries_and_defaults_to_the_documented_bound() {
        let run = " ".repeat(DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE);
        let mut scan = ArgumentScan::default();
        for delta in [r#"{"a":1"#, &run[..700], &run[700..], "}"] {
            assert_eq!(
                scan.check_delta(delta, DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE, "m", "edit"),
                Ok(())
            );
        }
        assert_eq!(scan.trailing_whitespace, 0);

        let mut overflowing = ArgumentScan::default();
        assert_eq!(
            overflowing.check_delta(
                &format!(r#"{{"a":1{run}"#),
                DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE,
                "m",
                "edit"
            ),
            Ok(())
        );
        assert!(
            overflowing
                .check_delta(" ", DEFAULT_MAX_TOOL_ARGUMENT_WHITESPACE, "m", "edit")
                .is_err()
        );
    }

    #[test]
    fn ignores_whitespace_inside_string_values_including_after_escaped_quotes_and_across_deltas() {
        let indentation = " ".repeat(5000);
        let mut scan = ArgumentScan::default();
        for delta in [
            r#"{"new_string":"\""#,
            &format!("{indentation}\\"),
            &format!("\"{indentation}\\\\"),
            &format!(r#"", "b":"{indentation}"}}"#),
        ] {
            assert_eq!(scan.check_delta(delta, 100, "m", "edit"), Ok(()));
        }
        assert!(!scan.in_string);
        assert_eq!(scan.trailing_whitespace, 0);
    }

    #[test]
    fn resets_the_run_on_any_character_outside_a_string() {
        let run = " ".repeat(90);
        let mut scan = ArgumentScan::default();
        for delta in [
            format!(r#"{{{run}"a"{run}:{run}[{run}1{run},{run}2{run}]{run}"#),
            format!(r#",{run}"b":{{{run}}}{run}}}"#),
        ] {
            assert_eq!(scan.check_delta(&delta, 100, "m", "edit"), Ok(()));
        }
    }
}
