//! Execution-free inventory: scan a component (recursing into nested
//! components/modules) for `component-test:tags@0.1` custom sections
//! and parse the newline-delimited `name mark1 mark2...` records.

use anyhow::{bail, Context as _};
use component_test_core::{name::is_wit_label, CaseName, Tag};
use wasmparser::{Parser, Payload};

use crate::lockfile::{CaseEntry, GeneratedEntry};

/// The custom-section name (canonical constant lives in core; the one
/// copy that cannot reference it is `component-test-sdk`'s `case!`
/// macro_rules literal).
pub use component_test_core::name::TAGS_SECTION;

/// Collect raw tags-section bytes from a component/module, nested
/// modules and components included.
pub fn collect_tags_sections(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    // `parse_all` descends into nested modules/components on its own.
    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::CustomSection(reader) = payload.context("parsing wasm")? {
            if reader.name() == TAGS_SECTION {
                out.extend_from_slice(reader.data());
                // Records are newline-delimited *within* a section, but
                // nothing guarantees a producer terminates its last
                // record; without this, records from adjacent sections
                // would fuse into one.
                if out.last() != Some(&b'\n') {
                    out.push(b'\n');
                }
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
                cases: Vec::new(),
            });
        } else {
            let name = CaseName::parse(name)
                .map_err(|e| anyhow::anyhow!("invalid case name `{name}` in tags section: {e}"))?;
            inv.cases.push(CaseEntry { name, tags });
        }
    }
    Ok(inv)
}

/// Static inventory of a suite component, distinguishing section
/// absence from corruption: `Ok(None)` means no tags section is
/// present (legitimate — suite not built with the SDK, or sections
/// stripped by composition; see findings #14), while `Err` means a
/// section exists but is malformed (always a harness bug — never
/// degrade it to "no inventory").
pub fn try_inventory(wasm: &[u8]) -> anyhow::Result<Option<Inventory>> {
    let sections = collect_tags_sections(wasm)?;
    if sections.is_empty() {
        return Ok(None);
    }
    let mut inv = parse_tags_records(&sections)?;
    // Section record order is linker-determined; canonicalize.
    inv.cases.sort_by(|a, b| a.name.cmp(&b.name));
    inv.generated.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    Ok(Some(inv))
}

/// Static inventory of a suite component; absence is an error.
pub fn inventory(wasm: &[u8]) -> anyhow::Result<Inventory> {
    try_inventory(wasm)?.with_context(|| {
        format!("no `{TAGS_SECTION}` section found (suite not built with the SDK, or sections stripped)")
    })
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

    /// Minimal core module: magic + version + the given custom
    /// sections (payloads small enough for single-byte LEB sizes).
    fn module_with_sections(sections: &[(&str, &[u8])]) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        for (name, data) in sections {
            let mut payload = vec![name.len() as u8];
            payload.extend_from_slice(name.as_bytes());
            payload.extend_from_slice(data);
            assert!(payload.len() < 0x80, "single-byte LEB only");
            wasm.push(0x00); // custom section id
            wasm.push(payload.len() as u8);
            wasm.extend_from_slice(&payload);
        }
        wasm
    }

    #[test]
    fn sections_do_not_fuse_without_trailing_newline() {
        let wasm = module_with_sections(&[
            (TAGS_SECTION, b"a/x".as_slice()), // no trailing newline
            (TAGS_SECTION, b"b/y hsm\nb/z !hsm\n".as_slice()),
        ]);
        let inv = try_inventory(&wasm).unwrap().unwrap();
        let names: Vec<String> = inv.cases.iter().map(|c| c.name.to_string()).collect();
        assert_eq!(names, ["a/x", "b/y", "b/z"]);
    }

    #[test]
    fn absent_section_is_none_but_malformed_is_err() {
        let bare = module_with_sections(&[]);
        assert!(try_inventory(&bare).unwrap().is_none());
        assert!(inventory(&bare).is_err());

        let bad = module_with_sections(&[(TAGS_SECTION, b"Bad/Name\n".as_slice())]);
        assert!(try_inventory(&bad).is_err());
    }
}
