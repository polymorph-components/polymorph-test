//! Execution-free inventory: scan a component (recursing into nested
//! components/modules) for `component-test:tags@0.1` custom sections
//! and parse the newline-delimited `name mark1 mark2...` records.

use anyhow::{bail, Context as _};
use component_test_core::{name::is_wit_label, CaseName, Tag};
use wasmparser::{Parser, Payload};

use crate::lockfile::{CaseEntry, GeneratedEntry};

pub const TAGS_SECTION: &str = "component-test:tags@0.1";

/// Collect raw tags-section bytes from a component/module, nested
/// modules and components included.
pub fn collect_tags_sections(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    // `parse_all` descends into nested modules/components on its own.
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CustomSection(reader) = payload.context("parsing wasm")? {
            if reader.name() == TAGS_SECTION {
                out.extend_from_slice(reader.data());
            }
        }
    }
    Ok(out)
}

/// Parse concatenated records into lockfile case entries, validating
/// grammar and duplicates.
/// The parsed static inventory: exact case records plus generated-row
/// prefix records (`prefix/* tag...`), whose leaves are enumerated at
/// run time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub cases: Vec<CaseEntry>,
    pub generated: Vec<GeneratedEntry>,
}

/// Parse concatenated records, validating grammar and duplicates.
/// Record forms: `name tag...` (exact) and `prefix/* tag...`
/// (generated row: every prefix segment must be a WIT label, since
/// leaves are appended below it).
pub fn parse_tags_records(bytes: &[u8]) -> anyhow::Result<Inventory> {
    let text = std::str::from_utf8(bytes).context("tags section is not UTF-8")?;
    let mut inv = Inventory::default();
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split(' ').filter(|p| !p.is_empty());
        let name = parts.next().context("empty record")?;
        let tags = parts
            .map(|m| {
                Tag::parse(m).map_err(|e| anyhow::anyhow!("invalid tag `{m}` on `{name}`: {e}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        component_test_core::Tags::new(tags.clone())
            .map_err(|e| anyhow::anyhow!("record `{name}`: {e}"))?;
        if !seen.insert(name.to_string()) {
            bail!("duplicate record `{name}` in tags section");
        }
        if let Some(prefix) = name.strip_suffix("/*") {
            for seg in prefix.split('/') {
                if let Some(reason) = is_wit_label(seg) {
                    bail!(
                        "generated-row prefix `{prefix}`: segment `{seg}` is not a WIT label ({reason})"
                    );
                }
            }
            inv.generated.push(GeneratedEntry {
                prefix: prefix.to_string(),
                tags,
            });
        } else {
            let name = CaseName::parse(name)
                .map_err(|e| anyhow::anyhow!("invalid case name `{name}` in tags section: {e}"))?;
            inv.cases.push(CaseEntry { name, tags });
        }
    }
    Ok(inv)
}

/// Static inventory of a suite component.
pub fn inventory(wasm: &[u8]) -> anyhow::Result<Inventory> {
    let sections = collect_tags_sections(wasm)?;
    if sections.is_empty() {
        bail!("no `{TAGS_SECTION}` section found (suite not built with the SDK, or sections stripped)");
    }
    let mut inv = parse_tags_records(&sections)?;
    // Section record order is linker-determined; canonicalize.
    inv.cases.sort_by(|a, b| a.name.cmp(&b.name));
    inv.generated.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        let inv = parse_tags_records(b"sample/math/add\nsample/hsm/attest hsm !sim\n").unwrap();
        assert_eq!(inv.cases.len(), 2);
        assert_eq!(inv.cases[1].tags.len(), 2);
        assert_eq!(inv.cases[1].tags[1].to_string(), "!sim");
    }

    #[test]
    fn parses_prefix_records() {
        let inv = parse_tags_records(b"aes-gcm/wycheproof/* aes-gcm\nplain/case\n").unwrap();
        assert_eq!(inv.cases.len(), 1);
        assert_eq!(inv.generated.len(), 1);
        assert_eq!(inv.generated[0].prefix, "aes-gcm/wycheproof");
        assert_eq!(inv.generated[0].tags[0].to_string(), "aes-gcm");
        assert!(parse_tags_records(b"a_b/* x\n").is_err());
        assert!(parse_tags_records(b"375/*\n").is_err());
    }

    #[test]
    fn rejects_bad_records() {
        assert!(parse_tags_records(b"Bad/Name\n").is_err());
        assert!(parse_tags_records(b"a/x\na/x\n").is_err());
        assert!(parse_tags_records(b"a/x Bad_Tag\n").is_err());
        assert!(parse_tags_records(b"a/x hsm !hsm\n").is_err());
    }
}
