//! Markdown matrix renderer: per-target case outcomes at a glance.
//! Rows are case-name prefix groups (a case's name minus its leaf
//! segment), columns are targets; a uniform cell renders one word, a
//! mixed cell renders counts. Groups where every cell is uniform
//! collapse to a single row with a case count. Failures (with detail +
//! diagnostics) and per-target summaries follow.

use std::collections::{BTreeMap, BTreeSet};

use crate::aggregate::Aggregate;
use crate::results::Status;

/// Group cases at the shallowest prefix whose members are uniform per
/// target: start from top-level prefixes and only split a group when
/// its statuses disagree somewhere.
fn top_group(case: &str) -> &str {
    match case.find('/') {
        Some(i) => &case[..i],
        None => case,
    }
}

/// Display vocabulary for matrix cells — deliberately *not*
/// [`Status::word`]: failures shout (upper case), expected outcomes
/// stay quiet, and `N/A` keeps cells narrow. This match is exhaustive
/// on purpose: adding a `Status` variant must force a decision here
/// (the enum is `#[non_exhaustive]` only for foreign crates).
fn word(status: Status) -> &'static str {
    match status {
        Status::Pass => "pass",
        Status::Fail => "FAIL",
        Status::Skipped => "skip",
        Status::NotReached => "NOT-REACHED",
        Status::NotApplicable => "N/A",
        Status::Deselected => "deselected",
    }
}

/// Render the cross-target matrix as markdown.
pub fn render(agg: &Aggregate) -> String {
    // Union of case names in sorted order, grouped by prefix.
    let cases: BTreeSet<&str> = agg
        .results
        .values()
        .flat_map(|m| m.keys().map(String::as_str))
        .collect();
    // Group at the top-level prefix: uniform groups render one word,
    // mixed groups render counts (specifics live in the Failures
    // section). Keeps the matrix at-a-glance for 10k-case corpora.
    let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for case in &cases {
        groups
            .entry(top_group(case).to_string())
            .or_default()
            .push(case);
    }

    let mut md = String::new();
    md.push_str("# Test matrix\n\n");

    if !agg.errors.is_empty() {
        md.push_str("## Validation errors\n\n");
        for e in &agg.errors {
            md.push_str(&format!("- {e}\n"));
        }
        md.push('\n');
    }
    if !agg.warnings.is_empty() {
        md.push_str("## Warnings\n\n");
        for w in &agg.warnings {
            md.push_str(&format!("- {w}\n"));
        }
        md.push('\n');
    }

    md.push_str("| Case |");
    for target in &agg.targets {
        md.push_str(&format!(" {target} |"));
    }
    md.push_str("\n| --- |");
    for _ in &agg.targets {
        md.push_str(" --- |");
    }
    md.push('\n');

    for (group, members) in &groups {
        // A cell is uniform when every member has the same status for
        // that target; the group collapses when all cells are uniform.
        let cells: Vec<Option<Status>> = agg
            .targets
            .iter()
            .map(|target| {
                let statuses: BTreeSet<&'static str> = members
                    .iter()
                    .filter_map(|case| agg.results.get(target).and_then(|m| m.get(*case)))
                    .map(|r| word(r.status))
                    .collect();
                match statuses.len() {
                    1 => members
                        .iter()
                        .find_map(|case| agg.results.get(target).and_then(|m| m.get(*case)))
                        .map(|r| r.status),
                    _ => None,
                }
            })
            .collect();
        if cells.iter().all(Option::is_some) {
            let label = if members.len() > 1 {
                format!("{group} ({} cases)", members.len())
            } else {
                members[0].to_string()
            };
            md.push_str(&format!("| {label} |"));
            for cell in &cells {
                md.push_str(&format!(" {} |", word(cell.unwrap())));
            }
            md.push('\n');
        } else {
            md.push_str(&format!("| {group} ({} cases) |", members.len()));
            for (target, uniform) in agg.targets.iter().zip(&cells) {
                if let Some(status) = uniform {
                    md.push_str(&format!(" {} |", word(*status)));
                    continue;
                }
                let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
                for case in members {
                    if let Some(r) = agg.results.get(target).and_then(|m| m.get(*case)) {
                        *counts.entry(word(r.status)).or_default() += 1;
                    }
                }
                if counts.is_empty() {
                    md.push_str(" — |");
                } else {
                    let cell = counts
                        .iter()
                        .map(|(w, n)| format!("{n} {w}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    md.push_str(&format!(" {cell} |"));
                }
            }
            md.push('\n');
        }
    }
    md.push('\n');

    md.push_str("## Failures\n\n");
    let mut any = false;
    for target in &agg.targets {
        let Some(results) = agg.results.get(target) else {
            continue;
        };
        for r in results.values() {
            if !matches!(r.status, Status::Fail | Status::NotReached) {
                continue;
            }
            any = true;
            md.push_str(&format!(
                "- `{target}` `{}` {}: {}\n",
                r.case,
                word(r.status),
                r.detail.as_deref().unwrap_or("(no detail)")
            ));
            for d in &r.diagnostics {
                md.push_str(&format!("  - diag: {d}\n"));
            }
            if !r.diagnostics_complete {
                md.push_str("  - (diagnostics truncated)\n");
            }
        }
    }
    if !any {
        md.push_str("None.\n");
    }
    md.push('\n');

    md.push_str("## Summary\n\n");
    for target in &agg.targets {
        let Some(results) = agg.results.get(target) else {
            md.push_str(&format!("- `{target}`: no results\n"));
            continue;
        };
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for r in results.values() {
            *counts.entry(word(r.status)).or_default() += 1;
        }
        let parts = counts
            .iter()
            .map(|(w, n)| format!("{n} {w}"))
            .collect::<Vec<_>>()
            .join(", ");
        md.push_str(&format!(
            "- `{target}`: {parts} ({} total)\n",
            results.len()
        ));
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::CaseResult;
    use std::collections::BTreeMap;

    fn result(case: &str, status: Status, detail: Option<&str>) -> (String, CaseResult) {
        (
            case.to_string(),
            CaseResult {
                case: case.into(),
                status,
                provenance: None,
                detail: detail.map(String::from),
                seed: None,
                duration_ms: None,
                diagnostics: vec![],
                diagnostics_complete: true,
            },
        )
    }

    #[test]
    fn snapshot() {
        let mut results: BTreeMap<String, BTreeMap<String, CaseResult>> = BTreeMap::new();
        results.insert(
            "native".into(),
            [
                result("aes-gcm/wycheproof/tc1", Status::Pass, None),
                result("aes-gcm/wycheproof/tc2", Status::Pass, None),
                result("probe/decline", Status::NotApplicable, Some("!hsm")),
            ]
            .into(),
        );
        let mut fail = result("aes-gcm/wycheproof/tc2", Status::Fail, Some("tag mismatch"));
        fail.1.diagnostics.push("expected 0xab, got 0xcd".into());
        results.insert(
            "sim".into(),
            [
                result("aes-gcm/wycheproof/tc1", Status::Pass, None),
                fail,
                result("probe/decline", Status::Pass, None),
            ]
            .into(),
        );
        let agg = Aggregate {
            targets: vec!["native".into(), "sim".into()],
            results,
            errors: vec![],
            warnings: vec![],
        };
        let md = render(&agg);
        let expected = "\
# Test matrix

| Case | native | sim |
| --- | --- | --- |
| aes-gcm (2 cases) | pass | 1 FAIL, 1 pass |
| probe/decline | N/A | pass |

## Failures

- `sim` `aes-gcm/wycheproof/tc2` FAIL: tag mismatch
  - diag: expected 0xab, got 0xcd

## Summary

- `native`: 1 N/A, 2 pass (3 total)
- `sim`: 1 FAIL, 2 pass (3 total)
";
        assert_eq!(md, expected, "rendered:\n{md}");
    }
}
