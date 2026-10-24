//! The brief's lines (`brief_*`): which message and arguments each rule
//! takes. Pure.

use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::review::{
    AnalysisState, Brief, BriefItem, BriefRule, DiffSide, IncompleteKind, LineAnchor, NoiseKind,
    SensitiveKind,
};

use crate::brief::{PROVIDED, lines};
use crate::testing::file_key;

fn brief(items: Vec<BriefItem>) -> Brief {
    Brief {
        items: Arc::from(items),
        omitted: 0,
        pending_providers: 0,
        analysis: AnalysisState::Idle,
    }
}

fn item(rule: BriefRule, file: Option<u32>, count: u32) -> BriefItem {
    BriefItem {
        rule,
        critical: false,
        file: file.map(file_key),
        line: None,
        count,
    }
}

fn en() -> Strings {
    Strings::of(Locale::En)
}

#[test]
fn brief_rules_with_a_path_take_the_path() {
    let rules = [
        (
            BriefRule::ChangedSinceSeen,
            "review-brief-changed-since-seen",
        ),
        (BriefRule::Drifted, "review-brief-drifted"),
        (BriefRule::EditFailed, "review-brief-edit-failed"),
        (
            BriefRule::Sensitive(SensitiveKind::AgentInstructions),
            "review-brief-sensitive-agent-instructions",
        ),
        (
            BriefRule::Sensitive(SensitiveKind::Ci),
            "review-brief-sensitive-ci",
        ),
        (
            BriefRule::Sensitive(SensitiveKind::Hooks),
            "review-brief-sensitive-hooks",
        ),
        (
            BriefRule::Sensitive(SensitiveKind::GitMetadata),
            "review-brief-sensitive-git-metadata",
        ),
        (
            BriefRule::Sensitive(SensitiveKind::Symlink),
            "review-brief-sensitive-symlink",
        ),
        (
            BriefRule::Sensitive(SensitiveKind::Executable),
            "review-brief-sensitive-executable",
        ),
        (
            BriefRule::Sensitive(SensitiveKind::Submodule),
            "review-brief-sensitive-submodule",
        ),
    ];
    let lines = lines(
        &brief(
            rules
                .iter()
                .map(|(rule, _)| item(rule.clone(), Some(4), 1))
                .collect(),
        ),
        &en(),
    );
    assert_eq!(lines.len(), rules.len());
    for (line, (_, key)) in lines.iter().zip(rules) {
        assert_eq!(line.text.message_id(), Some(key));
        assert!(
            line.text.as_str().contains("src/file_004.rs"),
            "{}: {}",
            key,
            line.text.as_str()
        );
    }
}

#[test]
fn brief_rules_with_a_count_take_the_count() {
    let rules = [
        (
            BriefRule::CommandOnly,
            "review-brief-command-only",
            "3 files changed by a command, not by an edit",
        ),
        (
            BriefRule::Deleted,
            "review-brief-deleted",
            "3 tracked files deleted",
        ),
        (
            BriefRule::Incomplete(IncompleteKind::Binary),
            "review-brief-incomplete-binary",
            "3 files shown without a full diff: binary",
        ),
        (
            BriefRule::Incomplete(IncompleteKind::TooLarge),
            "review-brief-incomplete-too-large",
            "3 files shown without a full diff: too large",
        ),
        (
            BriefRule::Noise(NoiseKind::Lockfile),
            "review-brief-noise-lockfile",
            "3 lockfiles folded",
        ),
        (
            BriefRule::Noise(NoiseKind::Generated),
            "review-brief-noise-generated",
            "3 generated files folded",
        ),
        (
            BriefRule::Noise(NoiseKind::Snapshot),
            "review-brief-noise-snapshot",
            "3 snapshots folded",
        ),
    ];
    let lines = lines(
        &brief(
            rules
                .iter()
                .map(|(rule, _, _)| item(rule.clone(), None, 3))
                .collect(),
        ),
        &en(),
    );
    for (line, (_, key, text)) in lines.iter().zip(rules) {
        assert_eq!(line.text.message_id(), Some(key));
        assert_eq!(line.text.as_str(), text);
    }
    let one = self::lines(&brief(vec![item(BriefRule::Deleted, None, 1)]), &en());
    assert_eq!(one[0].text.as_str(), "1 tracked file deleted");
}

#[test]
fn brief_provided_rules_fall_back_to_the_rule_as_written() {
    assert!(
        PROVIDED
            .iter()
            .all(|(provider, rule, _)| *provider != "unknown" || *rule != "x"),
        "the table has no entry for this pair"
    );
    let provided = BriefRule::Provided {
        provider: Arc::from("unknown"),
        rule: Arc::from("x"),
        args: Arc::from(vec![(Arc::<str>::from("path"), Arc::<str>::from("a.rs"))]),
    };
    let lines = lines(&brief(vec![item(provided, None, 1)]), &en());
    assert_eq!(lines[0].text.message_id(), Some("review-brief-provided"));
    assert_eq!(lines[0].text.as_str(), "\u{2068}x\u{2069}");
}

#[test]
fn brief_lines_keep_the_critical_flag_the_file_and_the_line() {
    let mut critical = item(BriefRule::Drifted, Some(1), 1);
    critical.critical = true;
    critical.line = Some(LineAnchor {
        side: DiffSide::New,
        line: 12,
    });
    let lines = lines(&brief(vec![critical]), &en());
    assert!(lines[0].critical);
    assert_eq!(lines[0].file, Some(file_key(1)));
    assert_eq!(lines[0].line.map(|l| l.line), Some(12));
    assert_eq!(lines[0].id, "drifted:src/file_001.rs");
}

#[test]
fn brief_zh_cn_has_its_own_text() {
    let zh = Strings::of(Locale::ZhCn);
    let lines = lines(&brief(vec![item(BriefRule::Deleted, None, 2)]), &zh);
    assert_eq!(lines[0].text.as_str(), "删除了 2 个受跟踪的文件");
}
