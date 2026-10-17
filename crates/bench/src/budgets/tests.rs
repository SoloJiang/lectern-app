use std::collections::BTreeSet;
use std::path::PathBuf;

use super::*;
use crate::stats::{Kind, applies};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn budgets_ids_are_unique() {
    let mut seen = BTreeSet::new();
    for budget in BUDGETS {
        assert!(seen.insert(budget.id), "{} twice", budget.id);
    }
}

#[test]
fn budgets_ids_are_crate_slash_name() {
    for budget in BUDGETS {
        let segments: Vec<&str> = budget.id.split('/').collect();
        assert!(
            (2..=3).contains(&segments.len()),
            "{}: <crate or category>/<name>[/<scenario>]",
            budget.id
        );
        for segment in segments {
            assert!(
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    || segment == "*",
                "{}: segment {segment:?}",
                budget.id
            );
        }
    }
}

#[test]
fn budgets_specific_entries_win_over_wildcards() {
    assert_eq!(
        lookup(BUDGETS, "plugin/terminal/activate").unwrap().id,
        "plugin/*/activate"
    );
    assert_eq!(
        lookup(BUDGETS, "plugin/project/resident").unwrap().id,
        "plugin/*/resident"
    );
    assert_eq!(
        lookup(BUDGETS, "plugin/project/resident_cloudprint")
            .unwrap()
            .id,
        "plugin/project/resident_cloudprint"
    );
    assert_eq!(
        lookup(BUDGETS, "lsp/jdtls/resident").unwrap().id,
        "lsp/*/resident"
    );
    assert_eq!(lookup(BUDGETS, "lsp/total").unwrap().id, "lsp/total");
    // A wildcard matches exactly one non-empty segment.
    assert!(lookup(BUDGETS, "plugin/activate").is_none());
    assert!(lookup(BUDGETS, "plugin/a/b/activate").is_none());
    assert!(lookup(BUDGETS, "plugin//activate").is_none());

    static TABLE: &[Budget] = &[
        share("p/*/x", RECORD, KERNEL, ACTIVE, Process),
        share("p/special/x", RECORD, KERNEL, ACTIVE, Process),
        share("p/*/*", RECORD, KERNEL, ACTIVE, Process),
    ];
    assert_eq!(lookup(TABLE, "p/special/x").unwrap().id, "p/special/x");
    assert_eq!(lookup(TABLE, "p/other/x").unwrap().id, "p/*/x");
    assert_eq!(lookup(TABLE, "p/other/y").unwrap().id, "p/*/*");
}

#[test]
fn budgets_checks_apply_to_one_kind_of_value() {
    for budget in BUDGETS {
        let kinds: Vec<Kind> = Kind::ALL
            .into_iter()
            .filter(|k| budget.checks.iter().all(|c| applies(c, *k)))
            .collect();
        assert!(
            !kinds.is_empty(),
            "{}: no kind of value takes all of {:?}",
            budget.id,
            budget.checks
        );
        for check in budget.checks {
            let count = matches!(check.stat, Stat::CountAbove(_));
            let count_limit = matches!(check.limit, Limit::CountAtMost(_));
            assert_eq!(count, count_limit, "{}: {check:?}", budget.id);
        }
    }
}

#[test]
fn budgets_top_tier_sources_are_tech_research_or_adrs() {
    for budget in BUDGETS.iter().filter(|b| b.tier == Tier::Top) {
        assert!(
            budget.source.starts_with("docs/design/tech-research.md")
                || budget.source.starts_with("docs/adr/"),
            "{}: {}",
            budget.id,
            budget.source
        );
    }
    for budget in BUDGETS.iter().filter(|b| b.tier == Tier::Share) {
        assert!(
            budget.source.starts_with("docs/design/") && budget.source.ends_with("#性能"),
            "{}: a share comes from a crate design's「性能」: {}",
            budget.id,
            budget.source
        );
    }
}

/// GitHub's heading anchor: lower case, punctuation dropped, spaces as
/// hyphens.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// The anchors of the ATX headings outside code fences.
fn anchors(markdown: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut fenced = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            let base = slug(&line[hashes..]);
            let mut anchor = base.clone();
            let mut n = 1;
            while !out.insert(anchor.clone()) {
                anchor = format!("{base}-{n}");
                n += 1;
            }
        }
    }
    out
}

#[test]
fn budgets_sources_exist_and_their_anchors_are_headings() {
    for budget in BUDGETS {
        let (file, anchor) = match budget.source.split_once('#') {
            Some((file, anchor)) => (file, Some(anchor)),
            None => (budget.source, None),
        };
        let path = repo_root().join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {}: {e}", budget.id, path.display()));
        if let Some(anchor) = anchor {
            assert!(
                anchors(&text).contains(anchor),
                "{}: no heading for #{anchor} in {file}",
                budget.id
            );
        }
    }
}

#[test]
fn budgets_slug_follows_github() {
    assert_eq!(slug(" 性能预算"), "性能预算");
    assert_eq!(slug(" `just bench` 与 CI"), "just-bench-与-ci");
    assert_eq!(
        slug(" 探针（`lectern_bench::probe`，`probe` feature）"),
        "探针lectern_benchprobeprobe-feature"
    );
    assert!(anchors("# A\n## A\n```\n# B\n```\n").contains("a-1"));
    assert!(!anchors("```\n# B\n```\n").contains("b"));
}

#[test]
fn budgets_belong_to_the_documented_targets() {
    for budget in BUDGETS {
        let expected = if budget.id == "app/bundle_size" || budget.id == "app/dmg_size" {
            Target::Package
        } else if budget.id.ends_with("_bytes") || budget.id == "git/resident" {
            Target::Memory
        } else if ["app/", "plugin/", "lsp/", "baseline/"]
            .iter()
            .any(|p| budget.id.starts_with(p))
        {
            Target::Process
        } else {
            Target::Time
        };
        assert_eq!(budget.target, expected, "{}", budget.id);
    }
}

#[test]
fn budgets_describe_their_checks() {
    let describe = |id: &str| lookup(BUDGETS, id).unwrap().describe();
    assert_eq!(
        describe("agents/parse_throughput"),
        "throughput ≥ 200 MB/s (median)"
    );
    assert_eq!(describe("app/cold_start"), "count > 20 ms ≤ 1, max ≤ 30 ms");
    assert_eq!(
        describe("platform/safe_stat"),
        "ratio ≤ 3 (median), median < 10 µs"
    );
    assert_eq!(describe("executor/spawn_10k"), "record");
    assert_eq!(describe("project/facts_contains/name"), "median < 1 µs");
}
