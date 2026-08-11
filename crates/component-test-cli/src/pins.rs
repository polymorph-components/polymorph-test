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
use clap::{Args, Subcommand};

/// This workspace's crates, as named in a consumer's Cargo.lock.
const CRATE_PREFIX: &str = "component-test";
/// The JS runner-core facade packaged at the repository root.
const JS_PACKAGE: &str = "@polymorph/component-test-js";

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub(crate) struct PinsArgs {
    #[command(subcommand)]
    command: Option<PinsCommand>,
    /// Consumer Cargo.lock: every git-sourced component-test-* crate's
    /// resolved commit is a pin
    #[arg(long, required = true, value_name = "Cargo.lock")]
    cargo_lock: Option<String>,
    /// npm/pnpm lockfile carrying the @polymorph/component-test-js pin
    /// (repeatable)
    #[arg(long = "js-lock", value_name = "lockfile")]
    js_locks: Vec<String>,
    /// Additionally require the single rev to be exactly this
    #[arg(long, value_name = "rev")]
    expect: Option<String>,
}

#[derive(Subcommand)]
enum PinsCommand {
    /// Rewrite the declared pins in place (the write half of the gate);
    /// lockfile regeneration follow-ups are printed, not run
    Bump(BumpArgs),
}

#[derive(Args)]
struct BumpArgs {
    /// The commit to pin (a full 40-hex hash)
    #[arg(value_name = "rev", value_parser = parse_rev)]
    rev: String,
    /// Cargo.toml with git-pinned component-test-* dependency lines
    /// (repeatable)
    #[arg(long = "cargo-toml", value_name = "Cargo.toml")]
    cargo_tomls: Vec<String>,
    /// package.json naming @polymorph/component-test-js (repeatable)
    #[arg(long = "package-json", value_name = "package.json")]
    package_jsons: Vec<String>,
    /// Workflow file with polymorph-test action refs (repeatable)
    #[arg(long = "workflow", value_name = "ci.yml")]
    workflows: Vec<String>,
}

fn parse_rev(s: &str) -> Result<String, String> {
    let rev = s.to_ascii_lowercase();
    if is_rev(&rev) {
        Ok(rev)
    } else {
        Err("rev must be a full commit hash (40 hex)".to_string())
    }
}

pub(crate) fn pins_cmd(args: &PinsArgs) -> anyhow::Result<()> {
    if let Some(PinsCommand::Bump(bump)) = &args.command {
        return bump_cmd(bump);
    }
    // Required by clap whenever no subcommand was given.
    let cargo_lock = args.cargo_lock.as_deref().expect("required by clap");

    // Every place a pin was found: (source description, rev).
    let mut findings: Vec<(String, String)> = Vec::new();

    let text =
        std::fs::read_to_string(cargo_lock).with_context(|| format!("reading {cargo_lock}"))?;
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

    for path in &args.js_locks {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let revs = js_lock_revs(&text);
        if revs.is_empty() {
            bail!("{path}: no rev pin found for {JS_PACKAGE}");
        }
        for rev in revs {
            findings.push((format!("{path}: {JS_PACKAGE}"), rev));
        }
    }

    if let Some(rev) = &args.expect {
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

/// `pins bump <rev>`: rewrite the *declared* pins in place — the write
/// half of the gate above. Lockfiles are deliberately not touched:
/// regenerating them belongs to cargo/npm/pnpm, so the follow-up
/// commands are printed instead of run, and the check half verifies
/// the result.
fn bump_cmd(args: &BumpArgs) -> anyhow::Result<()> {
    let rev = &args.rev;
    let (cargo_tomls, package_jsons, workflows) =
        (&args.cargo_tomls, &args.package_jsons, &args.workflows);
    if cargo_tomls.is_empty() && package_jsons.is_empty() && workflows.is_empty() {
        bail!("nothing to bump: name at least one --cargo-toml/--package-json/--workflow");
    }

    let mut cargo_crates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut touched_js = false;
    for path in cargo_tomls {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let (new, crates) = bump_cargo_toml(&text, rev);
        if crates.is_empty() {
            bail!("{path}: no git-pinned `{CRATE_PREFIX}-*` dependency lines found");
        }
        report(path, &crates.join(", "), text == new);
        cargo_crates.extend(crates);
        std::fs::write(path, new).with_context(|| format!("writing {path}"))?;
    }
    for path in package_jsons {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let (new, n) = bump_named_lines(&text, JS_PACKAGE, rev);
        if n == 0 {
            bail!("{path}: no rev-pinned {JS_PACKAGE} line found");
        }
        report(path, JS_PACKAGE, text == new);
        touched_js = true;
        std::fs::write(path, new).with_context(|| format!("writing {path}"))?;
    }
    for path in workflows {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let (new, n) = bump_action_refs(&text, rev);
        if n == 0 {
            bail!("{path}: no `{ACTION_PATH_ANCHOR}<action>@<rev>` refs found");
        }
        report(path, &format!("{n} action ref(s)"), text == new);
        std::fs::write(path, new).with_context(|| format!("writing {path}"))?;
    }

    println!("next steps (lockfiles are regenerated by their owners, not edited):");
    if !cargo_crates.is_empty() {
        let flags: Vec<String> = cargo_crates.iter().map(|c| format!("-p {c}")).collect();
        println!("  cargo update {}", flags.join(" "));
    }
    if touched_js {
        println!("  npm install   (or: pnpm install) in each edited package directory");
    }
    println!("  component-test pins --cargo-lock ... [--js-lock ...] --expect {rev}");
    Ok(())
}

fn report(path: &str, what: &str, noop: bool) {
    if noop {
        println!("{path}: {what} already at the requested rev");
    } else {
        println!("{path}: bumped {what}");
    }
}

/// Rewrite `rev = "<40-hex>"` on git-dependency lines naming a
/// `component-test-*` crate. Anchoring is on the crate name at the
/// start of the line (the single-line dependency form every consumer
/// uses); other lines pass through untouched.
fn bump_cargo_toml(text: &str, rev: &str) -> (String, Vec<String>) {
    let mut crates = Vec::new();
    let out = map_lines(text, |line| {
        let name = line.trim_start().split(['=', ' ']).next().unwrap_or("");
        if !name.starts_with(CRATE_PREFIX) || !line.contains("git") {
            return None;
        }
        let replaced = replace_bounded_revs(line, rev);
        if replaced != line {
            crates.push(name.to_string());
            return Some(replaced);
        }
        // Already at the rev: still a bump site (for `cargo update -p`).
        if line.contains(&format!("rev = \"{rev}\"")) {
            crates.push(name.to_string());
        }
        None
    });
    (out, crates)
}

/// Replace bounded 40-hex runs on lines naming `needle` (the JS
/// facade's `github:...#<rev>` spec, format-agnostic).
fn bump_named_lines(text: &str, needle: &str, rev: &str) -> (String, usize) {
    let mut n = 0;
    let out = map_lines(text, |line| {
        if !line.contains(needle) {
            return None;
        }
        let replaced = replace_bounded_revs(line, rev);
        if replaced != line || line.contains(rev) {
            n += 1;
        }
        (replaced != line).then_some(replaced)
    });
    (out, n)
}

/// The repository path anchor for workflow `uses:` refs. Unlike crate
/// and package names, a `uses:` ref necessarily embeds the repository
/// path, so a repository rename must edit these lines anyway.
const ACTION_PATH_ANCHOR: &str = "polymorph-test/actions/";

/// Rewrite `@<40-hex>` on `uses: .../polymorph-test/actions/...` lines.
fn bump_action_refs(text: &str, rev: &str) -> (String, usize) {
    let mut n = 0;
    let out = map_lines(text, |line| {
        if !line.contains(ACTION_PATH_ANCHOR) || !line.contains('@') {
            return None;
        }
        let replaced = replace_bounded_revs(line, rev);
        if replaced != line || line.contains(rev) {
            n += 1;
        }
        (replaced != line).then_some(replaced)
    });
    (out, n)
}

/// Apply a per-line rewrite, preserving untouched lines and the final
/// newline exactly.
fn map_lines(text: &str, mut f: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let (line, tail) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        match f(line) {
            Some(new) => out.push_str(&new),
            None => out.push_str(line),
        }
        if rest.len() > line.len() {
            out.push('\n');
        }
        rest = tail;
    }
    out
}

/// Replace every bounded 40-hex run in `line` with `rev` (the same
/// boundary rule as `collect_revs`: neighbors must not be
/// alphanumeric, so integrity strings survive).
fn replace_bounded_revs(line: &str, rev: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if !is_hex(bytes[i]) {
            out.push(bytes[i] as char);
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
            out.push_str(rev);
        } else {
            out.push_str(&line[start..i]);
        }
    }
    out
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

    fn cargo_toml(rev: &str) -> String {
        format!(
            r#"[workspace.dependencies]
# component-test-sdk pins the whole stack; bump deliberately.
component-test-sdk = {{ git = "https://github.com/polymorph-components/polymorph-test", rev = "{rev}" }}
component-test-runner = {{ git = "https://github.com/polymorph-components/polymorph-test", rev = "{rev}" }}
rustls-rustcrypto = {{ git = "https://github.com/RustCrypto/rustls-rustcrypto", rev = "7b44a2de957b6596d267a920d3aec0d4137defb1" }}
"#
        )
    }

    #[test]
    fn bump_cargo_toml_is_name_anchored() {
        let (out, crates) = bump_cargo_toml(&cargo_toml(OTHER), REV);
        assert_eq!(out, cargo_toml(REV));
        assert_eq!(
            crates,
            vec![
                "component-test-sdk".to_string(),
                "component-test-runner".to_string()
            ]
        );
        // The foreign git dep's rev survives; the comment line is not a
        // dependency line.
        assert!(out.contains("7b44a2de957b6596d267a920d3aec0d4137defb1"));
    }

    #[test]
    fn bump_cargo_toml_already_at_rev_is_a_noop_site() {
        let (out, crates) = bump_cargo_toml(&cargo_toml(REV), REV);
        assert_eq!(out, cargo_toml(REV));
        assert_eq!(crates.len(), 2);
    }

    #[test]
    fn bump_package_json_line() {
        let text = format!(
            "{{\n  \"dependencies\": {{\n    \"@polymorph/component-test-js\": \"github:polymorph-components/polymorph-test#{OTHER}\",\n    \"playwright-core\": \"^1.61.1\"\n  }}\n}}\n"
        );
        let (out, n) = bump_named_lines(&text, JS_PACKAGE, REV);
        assert_eq!(n, 1);
        assert!(out.contains(&format!("#{REV}\"")));
        assert!(!out.contains(OTHER));
    }

    #[test]
    fn bump_action_refs_leaves_the_grep_guard_alone() {
        let text = format!(
            "      - run: |\n          if grep -E 'polymorph-test/actions/aggregate@[0-9a-f]{{40}}' ci.yml; then exit 1; fi\n      - uses: polymorph-components/polymorph-test/actions/aggregate@{OTHER}\n      - uses: polymorph-components/polymorph-test/actions/setup@{OTHER}\n"
        );
        let (out, n) = bump_action_refs(&text, REV);
        assert_eq!(n, 2);
        assert_eq!(out.matches(REV).count(), 2);
        assert!(out.contains("[0-9a-f]{40}"), "regex text is not a rev");
        assert!(!out.contains(OTHER));
    }

    #[test]
    fn replace_bounded_revs_respects_boundaries() {
        assert_eq!(
            replace_bounded_revs(&format!("sha512-Z{OTHER}Q=="), REV),
            format!("sha512-Z{OTHER}Q==")
        );
        assert_eq!(
            replace_bounded_revs(&format!("x @{OTHER} y"), REV),
            format!("x @{REV} y")
        );
    }

    #[test]
    fn map_lines_preserves_final_newline_shape() {
        let with = "a\nb\n";
        let without = "a\nb";
        assert_eq!(map_lines(with, |_| None), with);
        assert_eq!(map_lines(without, |_| None), without);
    }
}
