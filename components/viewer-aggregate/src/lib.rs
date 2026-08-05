//! The viewer's aggregation engine: `component-test aggregate`'s exact
//! pipeline (parse lockfile + manifest, fold each stream with the
//! shared selection rule, join + validate) behind one JSON-out export.
//! The results viewer page runs this component, so its verdicts are the
//! gate's verdicts by construction — the "must not look greener than
//! the gate" invariant is structural, not a re-implementation promise.

use component_test_formats::{aggregate, lockfile::Lockfile, manifest::Manifest, results};
use serde_json::json;

wit_bindgen::generate!({
    world: "viewer-aggregate",
    path: "wit",
});

struct Component;

impl Guest for Component {
    fn run(
        lock: String,
        manifest: String,
        results: Vec<(String, String)>,
    ) -> Result<String, String> {
        run_impl(&lock, &manifest, &results).map_err(|e| format!("{e:#}"))
    }
}

fn run_impl(
    lock: &str,
    manifest: &str,
    result_streams: &[(String, String)],
) -> anyhow::Result<String> {
    let lf = Lockfile::from_toml(lock)?;
    let manifest = Manifest::from_toml(manifest)?;

    let mut docs = Vec::new();
    for (target, stream) in result_streams {
        let doc = results::fold_jsonl(
            stream,
            &component_test_formats::selected_names(Some(&lf), stream),
        )
        .map_err(|e| anyhow::anyhow!("folding results for target `{target}`: {e:#}"))?;
        docs.push((target.clone(), doc));
    }

    let agg = aggregate::aggregate(&lf, &manifest, &docs);

    // The CLI summary's exact accounting (component-test-cli main.rs).
    let mut failing = 0usize;
    let mut total = 0usize;
    let mut results_json = serde_json::Map::new();
    for (target, cases) in &agg.results {
        let mut cases_json = serde_json::Map::new();
        for (case, r) in cases {
            total += 1;
            let is_failing = agg.result_failing(target, r);
            if is_failing {
                failing += 1;
            }
            let mut v = serde_json::to_value(r)?;
            v.as_object_mut()
                .expect("CaseResult serializes to an object")
                .insert("failing".into(), json!(is_failing));
            cases_json.insert(case.clone(), v);
        }
        results_json.insert(target.clone(), cases_json.into());
    }

    let assessments: serde_json::Map<String, serde_json::Value> = agg
        .assessments
        .iter()
        .map(|(target, cases)| {
            let cases: serde_json::Map<String, serde_json::Value> = cases
                .iter()
                .map(|(case, a)| {
                    let v = match a {
                        aggregate::Assessment::ExpectedFail { reason, tracking } => json!({
                            "kind": "expected-fail",
                            "reason": reason,
                            "tracking": tracking,
                        }),
                        aggregate::Assessment::UnexpectedPass => {
                            json!({ "kind": "unexpected-pass" })
                        }
                    };
                    (case.clone(), v)
                })
                .collect();
            (target.clone(), cases.into())
        })
        .collect();

    let doc = json!({
        "targets": agg.targets,
        "results": results_json,
        "assessments": assessments,
        "errors": agg.errors,
        "warnings": agg.warnings,
        "summary": {
            "targets": agg.targets.len(),
            "results": total,
            "failing": failing,
            "expected-fail": agg.expected_fail_count(),
            "ok": agg.ok(),
        },
    });
    Ok(doc.to_string())
}

export!(Component);
