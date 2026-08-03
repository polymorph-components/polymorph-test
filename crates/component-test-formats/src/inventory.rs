//! Execution-free inventory: scan a component (recursing into nested
//! components/modules) for `component-test:marks@0.1` custom sections
//! and parse the newline-delimited `name mark1 mark2...` records.

use anyhow::{bail, Context as _};
use component_test_core::{CaseName, Mark};
use wasmparser::{Parser, Payload};

use crate::lockfile::CaseEntry;

pub const MARKS_SECTION: &str = "component-test:marks@0.1";

/// Collect raw marks-section bytes from a component/module, nested
/// modules and components included.
pub fn collect_marks_sections(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    // `parse_all` descends into nested modules/components on its own.
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CustomSection(reader) = payload.context("parsing wasm")? {
            if reader.name() == MARKS_SECTION {
                out.extend_from_slice(reader.data());
            }
        }
    }
    Ok(out)
}

/// Parse concatenated records into lockfile case entries, validating
/// grammar and duplicates.
pub fn parse_marks_records(bytes: &[u8]) -> anyhow::Result<Vec<CaseEntry>> {
    let text = std::str::from_utf8(bytes).context("marks section is not UTF-8")?;
    let mut entries = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split(' ').filter(|p| !p.is_empty());
        let name = parts.next().context("empty record")?;
        let name = CaseName::parse(name)
            .map_err(|e| anyhow::anyhow!("invalid case name `{name}` in marks section: {e}"))?;
        if !seen.insert(name.as_str().to_string()) {
            bail!("duplicate case `{name}` in marks section");
        }
        let marks = parts
            .map(|m| {
                Mark::parse(m)
                    .map_err(|e| anyhow::anyhow!("invalid mark `{m}` on `{name}`: {e}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        entries.push(CaseEntry { name, marks });
    }
    Ok(entries)
}

/// Static inventory of a suite component.
pub fn inventory(wasm: &[u8]) -> anyhow::Result<Vec<CaseEntry>> {
    let sections = collect_marks_sections(wasm)?;
    if sections.is_empty() {
        bail!("no `{MARKS_SECTION}` section found (suite not built with the SDK, or sections stripped)");
    }
    let mut entries = parse_marks_records(&sections)?;
    // Section record order is linker-determined; canonicalize.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        let entries =
            parse_marks_records(b"sample/math/add\nsample/hsm/attest hsm !sim\n").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].marks.len(), 2);
        assert_eq!(entries[1].marks[1].to_string(), "!sim");
    }

    #[test]
    fn rejects_bad_records() {
        assert!(parse_marks_records(b"Bad/Name\n").is_err());
        assert!(parse_marks_records(b"a/x\na/x\n").is_err());
        assert!(parse_marks_records(b"a/x Bad_Mark\n").is_err());
    }
}
