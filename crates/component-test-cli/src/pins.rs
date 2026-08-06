//! `pins`: the one-rev-everywhere gate for downstream consumers (#54).
//!
//! A consumer of this repository records the same git rev in several
//! places: the `component-test-*` crates resolved in `Cargo.lock`, the
//! `@polymorph/component-test-js` git dependency in an npm or pnpm
//! lockfile, and the rev its recipes derive to install the CLI. A
//! partial bump runs the JS harness against a Rust runner from a
//! different rev, so agreement is asserted mechanically. Anchoring is
//! on crate and package *names*: repository URLs change on renames —
//! which broke every downstream grep-based gate at once — names do not.

use anyhow::{bail, Context as _};

/// This workspace's crates, as named in a consumer's Cargo.lock.
const CRATE_PREFIX: &str = "component-test";
/// The JS runner-core facade packaged at the repository root.
const JS_PACKAGE: &str = "@polymorph/component-test-js";

pub(crate) fn pins_cmd(args: &[String]) -> anyhow::Result<()> {
    let mut cargo_lock = None;
    let mut js_locks: Vec<String> = Vec::new();
    let mut expect = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cargo-lock" => {
                cargo_lock = Some(it.next().context("--cargo-lock needs a path")?.clone())
            }
            "--js-lock" => js_locks.push(it.next().context("--js-lock needs a path")?.clone()),
            "--expect" => expect = Some(it.next().context("--expect needs a rev")?.clone()),
            "-h" | "--help" => {
                println!("{}", crate::usage());
                return Ok(());
            }
            other => bail!("unexpected argument `{other}`\n{}", crate::usage()),
        }
    }
    let cargo_lock = cargo_lock.context("missing --cargo-lock")?;

    // Every place a pin was found: (source description, rev).
    let mut findings: Vec<(String, String)> = Vec::new();

    let text =
        std::fs::read_to_string(&cargo_lock).with_context(|| format!("reading {cargo_lock}"))?;
    let crates = cargo_lock_revs(&text)?;
    if crates.is_empty() {
        bail!(
            "{cargo_lock}: no git-sourced `{CRATE_PREFIX}-*` packages \
             (nothing is rev-pinned — patched to a path?)"
        );
    }
    for (name, rev) in crates {
        findings.push((format!("{cargo_lock}: {name}"), rev));
    }

    for path in &js_locks {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let revs = js_lock_revs(&text);
        if revs.is_empty() {
            bail!("{path}: no rev pin found for {JS_PACKAGE}");
        }
        for rev in revs {
            findings.push((format!("{path}: {JS_PACKAGE}"), rev));
        }
    }

    if let Some(rev) = expect {
        findings.push(("--expect".into(), rev.to_ascii_lowercase()));
    }

    let revs: std::collections::BTreeSet<&str> =
        findings.iter().map(|(_, rev)| rev.as_str()).collect();
    if revs.len() > 1 {
        for (source, rev) in &findings {
            eprintln!("  {rev}  {source}");
        }
        bail!(
            "pin skew: {} distinct revs across {} pins",
            revs.len(),
            findings.len()
        );
    }
    // Non-empty: the Cargo.lock side bailed above when it found nothing.
    let rev = revs.iter().next().unwrap();
    println!("ok: one rev everywhere: {rev} ({} pins)", findings.len());
    Ok(())
}

/// `component-test-*` packages with git sources in a Cargo.lock, as
/// (crate name, commit). The source URL's fragment is cargo's resolved
/// commit and is always the full hash; the `?rev=` query may be
/// abbreviated, so the fragment wins. A git source carrying neither is
/// an error, not a skip: the crate is from this repository and its pin
/// must be checkable.
fn cargo_lock_revs(text: &str) -> anyhow::Result<Vec<(String, String)>> {
    let doc: toml::Table = toml::from_str(text).context("parsing Cargo.lock as TOML")?;
    let packages = doc
        .get("package")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or_default();
    let mut out = Vec::new();
    for pkg in packages {
        let Some(name) = pkg.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !name.starts_with(CRATE_PREFIX) {
            continue;
        }
        let Some(source) = pkg.get("source").and_then(|v| v.as_str()) else {
            // Workspace-local or [patch]ed-to-path packages have no
            // source; they are not pins.
            continue;
        };
        if !source.starts_with("git+") {
            continue;
        }
        let fragment = source
            .rsplit_once('#')
            .map(|(_, f)| f)
            .filter(|f| is_rev(f));
        match fragment.or_else(|| rev_param(source)) {
            Some(rev) => out.push((name.to_string(), rev.to_string())),
            None => bail!("{name}: git source without a resolvable commit: {source}"),
        }
    }
    Ok(out)
}

/// The `?rev=` query parameter of a git source URL, when it is a full
/// commit hash.
fn rev_param(source: &str) -> Option<&str> {
    let (_, tail) = source.split_once("?rev=")?;
    let rev = &tail[..tail.find(['#', '&']).unwrap_or(tail.len())];
    is_rev(rev).then_some(rev)
}

/// Name-anchored block scan for the JS package's pinned rev,
/// format-agnostic across npm's package-lock.json and pnpm-lock.yaml:
/// both are line-oriented and indentation-nested, and both record the
/// rev (spec fragment `#<rev>`, resolved git URL, codeload tarball)
/// either on a line naming the package or within its indented block.
/// Every hash found is returned; disagreement within one file surfaces
/// as pin skew at the caller.
fn js_lock_revs(text: &str) -> Vec<String> {
    let mut revs = std::collections::BTreeSet::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(JS_PACKAGE) {
            continue;
        }
        let anchor = indent(line);
        collect_revs(line, &mut revs);
        for l in &lines[i + 1..] {
            if l.trim().is_empty() {
                continue;
            }
            if indent(l) <= anchor {
                break;
            }
            collect_revs(l, &mut revs);
        }
    }
    revs.into_iter().collect()
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Collect maximal `[0-9a-f]` runs of exactly 40 bytes whose neighbors
/// are not alphanumeric. The boundary requirement rejects hex-looking
/// windows inside longer tokens (base64 integrity strings).
fn collect_revs(line: &str, out: &mut std::collections::BTreeSet<String>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !is_hex(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_hex(bytes[i]) {
            i += 1;
        }
        let bounded_left = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let bounded_right = i == bytes.len() || !bytes[i].is_ascii_alphanumeric();
        if i - start == 40 && bounded_left && bounded_right {
            out.insert(line[start..i].to_string());
        }
    }
}

fn is_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

/// A full git commit hash: exactly 40 lowercase hex bytes.
fn is_rev(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(is_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "b80c13be7b9fecfed8ec10a91d23d8cf8349defe";
    const OTHER: &str = "1917446e19c9e84cd5b9ad8def56d924f60adf61";

    fn cargo_lock(rev: &str) -> String {
        format!(
            r#"
version = 4

[[package]]
name = "anyhow"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "component-test-runner"
version = "0.1.0"
source = "git+https://github.com/polymorph-components/polymorph-test?rev={rev}#{rev}"

[[package]]
name = "component-test-sdk"
version = "0.1.0"
source = "git+https://github.com/polymorph-components/polymorph-test?rev={rev}#{rev}"

[[package]]
name = "component-test-local"
version = "0.1.0"
"#
        )
    }

    #[test]
    fn cargo_lock_extraction_is_name_anchored() {
        let revs = cargo_lock_revs(&cargo_lock(REV)).unwrap();
        assert_eq!(
            revs,
            vec![
                ("component-test-runner".into(), REV.to_string()),
                ("component-test-sdk".into(), REV.to_string()),
            ]
        );
        // A renamed repository changes the URL, not the crate names.
        let renamed = cargo_lock(REV).replace("polymorph-components/polymorph-test", "elsewhere/x");
        assert_eq!(cargo_lock_revs(&renamed).unwrap().len(), 2);
    }

    #[test]
    fn cargo_lock_short_rev_resolves_via_fragment() {
        let lock = cargo_lock(REV).replace(&format!("?rev={REV}"), "?rev=b80c13be");
        let revs = cargo_lock_revs(&lock).unwrap();
        assert!(revs.iter().all(|(_, r)| r == REV));
    }

    #[test]
    fn npm_package_lock_block_scan() {
        let lock = format!(
            r#"{{
  "name": "jco",
  "lockfileVersion": 3,
  "packages": {{
    "": {{
      "dependencies": {{
        "@polymorph/component-test-js": "github:polymorph-components/polymorph-test#{REV}"
      }}
    }},
    "node_modules/@polymorph/component-test-js": {{
      "version": "0.2.0",
      "resolved": "git+ssh://git@github.com/polymorph-components/polymorph-test.git#{REV}",
      "integrity": "sha512-FSOFPIQsMi5E83WwZvRrRiU6UBZ1WhegHCPOg3Zly7ntycw19KXNN9Vm8Gxjc1E6iL22DkL18aY8huZnfMo3hQ=="
    }}
  }}
}}
"#
        );
        assert_eq!(js_lock_revs(&lock), vec![REV.to_string()]);
    }

    #[test]
    fn pnpm_lock_block_scan() {
        let lock = format!(
            r#"lockfileVersion: '9.0'

importers:
  .:
    dependencies:
      '@polymorph/component-test-js':
        specifier: github:polymorph-components/polymorph-test#{REV}
        version: https://codeload.github.com/polymorph-components/polymorph-test/tar.gz/{REV}

packages:
  '@polymorph/component-test-js@https://codeload.github.com/polymorph-components/polymorph-test/tar.gz/{REV}':
    resolution: {{gitHosted: true, integrity: sha512-FSOFPIQsMi5E83WwZvRrRiU6UBZ1WhegHCPOg3Zly7ntycw19KXNN9Vm8Gxjc1E6iL22DkL18aY8huZnfMo3hQ==, tarball: https://codeload.github.com/polymorph-components/polymorph-test/tar.gz/{REV}}}
"#
        );
        assert_eq!(js_lock_revs(&lock), vec![REV.to_string()]);
    }

    #[test]
    fn skewed_js_lock_yields_both_revs() {
        let lock = format!(
            "  '@polymorph/component-test-js':\n    specifier: github:x/y#{REV}\n    version: https://codeload.github.com/x/y/tar.gz/{OTHER}\n"
        );
        assert_eq!(
            js_lock_revs(&lock),
            vec![OTHER.to_string(), REV.to_string()]
        );
    }

    #[test]
    fn scan_stops_at_dedent() {
        let lock = format!(
            "  '@polymorph/component-test-js':\n    specifier: github:x/y#{REV}\n  'other-package':\n    version: https://codeload.github.com/x/y/tar.gz/{OTHER}\n"
        );
        assert_eq!(js_lock_revs(&lock), vec![REV.to_string()]);
    }

    #[test]
    fn embedded_and_offsize_hex_rejected() {
        let mut revs = std::collections::BTreeSet::new();
        // 40 hex embedded in a longer alphanumeric token.
        collect_revs(&format!("integrity: sha512-Z{REV}Q=="), &mut revs);
        // 39 and 41 bytes.
        collect_revs(&format!("x: {}", &REV[1..]), &mut revs);
        collect_revs(&format!("x: {REV}f"), &mut revs);
        assert!(revs.is_empty());
        collect_revs(&format!("tarball: https://c/tar.gz/{REV}"), &mut revs);
        assert_eq!(revs.len(), 1);
    }
}
