//! Single-process test arm for the evaluator's ordinary_edit fixture, not an agent.

use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

const SOURCE: &str = "function roundMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\nmodule.exports = { roundMoney };\n";
const MAX_PROMPT_BYTES: u64 = 64 * 1024;

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("expected a fixture mode")?;
    if args.next().is_some()
        || !matches!(
            mode.as_str(),
            "edit" | "tamper-tests" | "claim-only" | "edit-exit1"
        )
    {
        return Err("expected edit, tamper-tests, claim-only, or edit-exit1".into());
    }

    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_PROMPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_PROMPT_BYTES {
        return Err("expected 1..65536 prompt bytes on stdin".into());
    }
    let prompt = String::from_utf8(bytes)?;

    // Refuse a workspace other than the untouched fixture before making edits.
    if fs::read_to_string("src/money.js")? != SOURCE || !fs::metadata("test.cjs")?.is_file() {
        return Err("expected the ordinary_edit fixture".into());
    }
    if mode != "claim-only" {
        fs::write("src/money.js", SOURCE.replace("Math.floor(", "Math.round("))?;
    }
    if mode == "tamper-tests" {
        fs::write("test.cjs", "console.log('FIXTURE_PASS');\n")?;
    }

    let mut stdout = io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &serde_json::json!({ "mode": mode, "prompt": prompt }),
    )?;
    stdout.write_all(b"\nFIXTURE_PASS\n")?;
    Ok(if mode == "edit-exit1" {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("bake-eval-fake-arm: {error}");
            ExitCode::from(2)
        }
    }
}
