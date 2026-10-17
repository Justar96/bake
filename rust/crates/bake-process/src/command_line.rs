//! The Windows command line for an argument vector, as
//! `CommandLineToArgvW` (and the Microsoft C runtime) reads it back. The
//! rule is `bake-sandbox`'s `quote_arg` and `build_command_line`, from Bake's
//! TypeScript `quoteArg` (`packages/subprocess/win32-process/src/process.ts`);
//! it is kept here so the owner needs no sandbox dependency. Pure, so it is
//! compiled and tested on every platform.
#![cfg_attr(
    not(windows),
    allow(dead_code, reason = "Windows spawns use it; tested everywhere")
)]

/// Appends `argument` to a command line, as UTF-16 code units.
fn quote_arg(argument: &[u16], out: &mut Vec<u16>) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let needs_quotes = argument.is_empty()
        || argument
            .iter()
            .any(|&unit| matches!(unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x0c | 0x0d | QUOTE));
    if !needs_quotes {
        out.extend_from_slice(argument);
        return;
    }
    out.push(QUOTE);
    let mut index = 0;
    loop {
        let mut backslashes = 0;
        while index < argument.len() && argument[index] == BACKSLASH {
            backslashes += 1;
            index += 1;
        }
        if index == argument.len() {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
            break;
        }
        if argument[index] == QUOTE {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
            out.push(QUOTE);
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            out.push(argument[index]);
        }
        index += 1;
    }
    out.push(QUOTE);
}

/// The command line for `argv`: each entry quoted, one space apart, without
/// a terminating NUL.
pub(crate) fn build_command_line<A: AsRef<[u16]>>(argv: &[A]) -> Vec<u16> {
    let mut line = Vec::new();
    for (index, argument) in argv.iter().enumerate() {
        if index > 0 {
            line.push(u16::from(b' '));
        }
        quote_arg(argument.as_ref(), &mut line);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        let units: Vec<Vec<u16>> = args
            .iter()
            .map(|arg| arg.encode_utf16().collect())
            .collect();
        String::from_utf16(&build_command_line(&units)).expect("UTF-16")
    }

    // The cases of `bake-sandbox`'s `windows_acl` quoting test, and the MCP
    // stdio module's, which this replaces.
    #[test]
    fn quotes_as_command_line_to_argv_reads_back() {
        for (input, expected) in [
            ("", "\"\""),
            ("plain", "plain"),
            ("a b", "\"a b\""),
            ("a\"b", "\"a\\\"b\""),
            ("a\\b", "a\\b"),
            ("a b\\", "\"a b\\\\\""),
            ("a b\\\\", "\"a b\\\\\\\\\""),
            ("a\\\\\"b", "\"a\\\\\\\\\\\"b\""),
            ("tab\there", "\"tab\there\""),
        ] {
            assert_eq!(line(&[input]), expected, "{input:?}");
        }
        assert_eq!(
            line(&[r"C:\Program Files\tool.exe", "a b", "c"]),
            r#""C:\Program Files\tool.exe" "a b" c"#
        );
        assert_eq!(line(&["a", "b c", ""]), r#"a "b c" """#);
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r"dir\ with\", r"x\"]), r#""dir\ with\\" x\"#);
    }
}
