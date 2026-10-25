//! The oldest glibc a built Linux `bake` runs on.
//!
//! The dynamic loader refuses to start a binary whose version needs
//! (`.gnu.version_r`) name a glibc symbol version the host's glibc lacks,
//! unless the need is weak: Rust's standard library binds newer functions
//! such as `pidfd_spawnp` weakly and falls back when they are missing. This
//! reads those needs with `readelf --dynamic --version-info`, reports the
//! highest non-weak `GLIBC_` version, which is the binary's real floor, and
//! the highest weak one, and fails when the floor is above the limit,
//! explaining it from the symbols `readelf --dyn-syms` lists.
//!
//! It fails closed: readelf runs in the C locale, since its headings are
//! translated, and a dynamically linked binary whose glibc needs cannot be
//! read is an error, not a pass. The default limit, [`DEFAULT_FLOOR`], is
//! the floor the release owner chose (D50): glibc 2.34.

use std::cmp::Ordering;
use std::path::Path;
use std::process::Command;

use crate::util::Result;

/// The highest glibc version a release binary may need.
pub const DEFAULT_FLOOR: &str = "GLIBC_2.34";

/// One glibc symbol version a binary needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub name: String,
    /// Whether the loader starts the binary without it.
    pub weak: bool,
}

/// A glibc symbol a binary imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub version: String,
    pub weak: bool,
}

/// The numbers of a `GLIBC_<major>.<minor>[.<patch>]` name.
fn numbers(value: &str) -> Option<Vec<u64>> {
    let rest = value.strip_prefix("GLIBC_")?;
    rest.split('.')
        .map(|part| {
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        })
        .collect()
}

/// Whether `value` names a glibc symbol version, such as `GLIBC_2.35`.
pub fn is_glibc_version(value: &str) -> bool {
    numbers(value).is_some()
}

/// Orders two glibc versions by their numbers; a malformed one sorts first.
pub fn compare(left: &str, right: &str) -> Ordering {
    let (a, b) = (
        numbers(left).unwrap_or_default(),
        numbers(right).unwrap_or_default(),
    );
    for index in 0..a.len().max(b.len()) {
        let order = a
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(index).copied().unwrap_or(0));
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

/// The glibc version needs in `readelf --dynamic --version-info` output.
///
/// # Errors
///
/// When the output reports no dynamic section either way, or a
/// dynamically linked binary shows no glibc version need.
pub fn parse_version_needs(output: &str) -> Result<Vec<Need>> {
    let lines: Vec<&str> = output.lines().collect();
    if !lines.iter().any(|line| {
        line.starts_with("Dynamic section at offset ")
            || line.starts_with("There is no dynamic section in this file.")
    }) {
        return Err(
            "readelf printed no dynamic section report, so whether the binary loads glibc is unknown"
                .to_owned(),
        );
    }
    let linked = lines
        .iter()
        .filter(|line| line.contains("(NEEDED)") && line.contains("Shared library: ["))
        .count();
    let mut needs = Vec::new();
    let mut in_needs = false;
    for line in &lines {
        if line.starts_with("Version needs section ") {
            in_needs = true;
            continue;
        }
        if line
            .chars()
            .next()
            .is_some_and(|first| !first.is_whitespace())
        {
            in_needs = false;
        }
        if !in_needs {
            continue;
        }
        let Some(at) = line.find("Name: ") else {
            continue;
        };
        let rest = &line[at + "Name: ".len()..];
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        let Some(flags_at) = rest.find("Flags: ") else {
            continue;
        };
        let flags = &rest[flags_at + "Flags: ".len()..];
        let flags = flags.split("Version:").next().unwrap_or("");
        if !is_glibc_version(name) || !rest.contains("Version: ") {
            continue;
        }
        needs.push(Need {
            name: name.to_owned(),
            weak: flags.split_whitespace().any(|flag| flag == "WEAK"),
        });
    }
    if linked > 0 && needs.is_empty() {
        return Err(format!(
            "the binary loads {linked} shared {} but readelf shows no glibc version need, so its glibc floor is unknown",
            if linked == 1 { "library" } else { "libraries" }
        ));
    }
    Ok(needs)
}

/// The undefined glibc symbols in `readelf --dyn-syms --wide` output:
/// `FUNC WEAK DEFAULT UND pidfd_spawnp@GLIBC_2.39 (21)`.
pub fn parse_symbols(output: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Num: Value Size Type Bind Vis Ndx Name
        if fields.len() < 8 || !fields[0].ends_with(':') || fields[6] != "UND" {
            continue;
        }
        let Some((name, version)) = fields[7].split_once('@') else {
            continue;
        };
        let version = version.trim_start_matches('@');
        if !is_glibc_version(version) {
            continue;
        }
        symbols.push(Symbol {
            name: name.to_owned(),
            version: version.to_owned(),
            weak: fields[4] == "WEAK",
        });
    }
    symbols
}

/// The highest required and the highest weak version above it.
pub fn floor(needs: &[Need]) -> (Option<String>, Option<String>) {
    let highest = |weak: bool| {
        needs
            .iter()
            .filter(|need| need.weak == weak)
            .map(|need| need.name.clone())
            .max_by(|a, b| compare(a, b))
    };
    let required = highest(false);
    let weak = highest(true).filter(|weak| {
        required
            .as_deref()
            .is_none_or(|required| compare(weak, required) == Ordering::Greater)
    });
    (required, weak)
}

/// The report line and whether the floor is within `limit`.
pub fn check(binary: &str, needs: &[Need], limit: &str) -> Result<(bool, String)> {
    if !is_glibc_version(limit) {
        return Err(format!(
            "--floor {limit:?} is not a glibc version such as {DEFAULT_FLOOR}"
        ));
    }
    let (required, weak) = floor(needs);
    let weak_note = weak.as_ref().map_or_else(String::new, |weak| {
        format!("; weak references up to {weak} are used only where the host has them")
    });
    Ok(match required {
        None if weak.is_none() => (
            true,
            format!("{binary} is statically linked and needs no glibc symbol version"),
        ),
        None => (
            true,
            format!("{binary} needs no glibc symbol version it cannot start without{weak_note}"),
        ),
        Some(required) => {
            let ok = compare(&required, limit) != Ordering::Greater;
            let verdict = if ok {
                format!("within the {limit} floor")
            } else {
                format!(
                    "above the {limit} floor, so it does not start on a host with glibc {}",
                    &limit["GLIBC_".len()..]
                )
            };
            (
                ok,
                format!("{binary} needs {required}, {verdict}{weak_note}"),
            )
        }
    })
}

/// Why a binary needs `required` above `limit`.
pub fn explain(symbols: &[Symbol], required: &str, limit: &str) -> Option<String> {
    let named = |list: Vec<&Symbol>| {
        let mut list = list;
        list.sort_by(|a, b| compare(&b.version, &a.version).then_with(|| a.name.cmp(&b.name)));
        list.iter()
            .map(|symbol| format!("{}@{}", symbol.name, symbol.version))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let strong: Vec<&Symbol> = symbols
        .iter()
        .filter(|symbol| !symbol.weak && compare(&symbol.version, limit) == Ordering::Greater)
        .collect();
    if !strong.is_empty() {
        return Some(format!(
            "Bound above {limit} without a weak fallback: {}",
            named(strong)
        ));
    }
    let weak: Vec<&Symbol> = symbols
        .iter()
        .filter(|symbol| symbol.version == required)
        .collect();
    if weak.is_empty() {
        return None;
    }
    Some(format!(
        "Every symbol bound at {required} is weak ({}), but the linker did not mark the {required} version need weak, so the loader requires it anyway",
        named(weak)
    ))
}

/// Runs readelf on `binary` in the C locale.
fn readelf(args: &[&str], binary: &Path) -> Result<String> {
    let output = Command::new("readelf")
        .args(args)
        .arg("--wide")
        .arg(binary)
        .env_remove("LANGUAGE")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .map_err(|_| {
            "readelf, from binutils, is needed to read the binary's glibc version needs".to_owned()
        })?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() || !stderr.is_empty() {
        return Err(format!(
            "readelf could not read {}: {}",
            binary.display(),
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Checks `binary` against `limit`, printing the report and the
/// explanation; `Ok(false)` when the floor is too high.
pub fn run(binary: &Path, limit: &str) -> Result<bool> {
    let needs = parse_version_needs(&readelf(&["--dynamic", "--version-info"], binary)?)
        .map_err(|error| format!("{}: {error}", binary.display()))?;
    let (ok, report) = check(&binary.display().to_string(), &needs, limit)?;
    println!("{report}");
    if !ok
        && let (Some(required), _) = floor(&needs)
        && let Some(explanation) = explain(
            &parse_symbols(&readelf(&["--dyn-syms"], binary)?),
            &required,
            limit,
        )
    {
        println!("{explanation}");
    }
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    const READELF: &str = "\
Dynamic section at offset 0x3ce0d8 contains 30 entries:
  Tag        Type                         Name/Value
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [ld-linux-x86-64.so.2]

Version symbols section '.gnu.version' contains 200 entries:
 Addr: 0x0000000000001234  Offset: 0x001234  Link: 6 (.dynsym)
  000:   0 (*local*)       2 (GLIBC_2.2.5)

Version needs section '.gnu.version_r' contains 2 entries:
 Addr: 0x0000000000004d48  Offset: 0x004d48  Link: 7 (.dynstr)
  000000: Version: 1  File: ld-linux-x86-64.so.2  Cnt: 1
  0x0010:   Name: GLIBC_2.3  Flags: none  Version: 9
  0x0020: Version: 1  File: libc.so.6  Cnt: 3
  0x0030:   Name: GLIBC_2.39  Flags: WEAK   Version: 14
  0x0040:   Name: GLIBC_2.34  Flags: none  Version: 12
  0x0050:   Name: GLIBC_2.2.5  Flags: none  Version: 2
";

    #[test]
    fn the_floor_is_the_highest_strong_need() {
        let needs = parse_version_needs(READELF).expect("needs");
        assert_eq!(needs.len(), 4);
        assert_eq!(
            floor(&needs),
            (Some("GLIBC_2.34".to_owned()), Some("GLIBC_2.39".to_owned()))
        );
        let (ok, report) = check("bake", &needs, DEFAULT_FLOOR).expect("check");
        assert!(ok, "{report}");
        assert!(report.contains("needs GLIBC_2.34, within the GLIBC_2.34 floor"));
        let (ok, report) = check("bake", &needs, "GLIBC_2.28").expect("check");
        assert!(!ok);
        assert!(report.contains("above the GLIBC_2.28 floor"), "{report}");
        assert!(check("bake", &needs, "2.34").is_err());
    }

    #[test]
    fn unreadable_needs_fail_closed() {
        assert!(parse_version_needs("Dynamische Sektion").is_err());
        let linked_without_needs = "Dynamic section at offset 0x1 contains 2 entries:\n 0x1 (NEEDED)             Shared library: [libc.so.6]\n";
        assert!(parse_version_needs(linked_without_needs).is_err());
        let statically = "There is no dynamic section in this file.\n";
        let needs = parse_version_needs(statically).expect("static");
        assert!(check("bake", &needs, DEFAULT_FLOOR).expect("check").0);
    }

    #[test]
    fn the_explanation_names_the_symbols() {
        let symbols = parse_symbols(
            "    12: 0000000000000000     0 FUNC    WEAK   DEFAULT  UND pidfd_spawnp@GLIBC_2.39 (14)\n    13: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND memcpy@GLIBC_2.14 (3)\n    14: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND newer@GLIBC_2.36 (4)\n",
        );
        assert_eq!(symbols.len(), 3);
        assert_eq!(
            explain(&symbols, "GLIBC_2.36", DEFAULT_FLOOR).as_deref(),
            Some("Bound above GLIBC_2.34 without a weak fallback: newer@GLIBC_2.36")
        );
        let weak_only = &symbols[..1];
        assert!(
            explain(weak_only, "GLIBC_2.39", DEFAULT_FLOOR)
                .expect("weak")
                .contains("did not mark the GLIBC_2.39 version need weak")
        );
        assert_eq!(compare("GLIBC_2.4", "GLIBC_2.17"), Ordering::Less);
        assert_eq!(compare("GLIBC_2.2.5", "GLIBC_2.2"), Ordering::Greater);
    }
}
