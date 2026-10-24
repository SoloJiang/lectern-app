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

// ----- the usage rows (step 2) ---------------------------------------------------

mod usage {
    use std::sync::Arc;

    use lectern_i18n::{Locale, Strings};
    use lectern_services::project::WorktreeKey;
    use lectern_services::usage::{
        BranchLabel, ForgeRepo, MrLabel, MrRef, MrState, RowKey, SessionRow, SessionUsage,
        SnapshotHeader, Tokens, UsageState, UseId,
    };

    use crate::brief::{turn_tokens_text, usage_lines};

    /// The text without the isolation marks Fluent puts around arguments.
    fn plain(text: &lectern_ui::Text) -> String {
        text.as_str().replace(['\u{2068}', '\u{2069}'], "")
    }

    fn tokens(n: u64) -> Tokens {
        Tokens {
            input: n,
            ..Tokens::default()
        }
    }

    fn worktree(path: &str) -> WorktreeKey {
        WorktreeKey(Arc::from(std::path::Path::new(path)))
    }

    fn branch(name: &str, at: &str) -> BranchLabel {
        BranchLabel {
            name: Arc::from(name),
            worktree: Some(worktree(at)),
        }
    }

    fn row(key: RowKey, at: &str, cumulative: u64, part: u64, shared: bool) -> SessionRow {
        SessionRow {
            key,
            worktree: Some(worktree(at)),
            cumulative: tokens(cumulative),
            session_part: tokens(part),
            shared,
        }
    }

    fn usage(rows: Vec<SessionRow>) -> SessionUsage {
        SessionUsage {
            header: SnapshotHeader {
                generation: 1,
                state: UsageState::Ready,
                may_include_inherited: false,
            },
            total: tokens(0),
            rows: Arc::from(rows),
            turns: Arc::from(Vec::new()),
        }
    }

    fn on_branch(name: &str, at: &str, cumulative: u64, part: u64) -> SessionRow {
        row(
            RowKey::Branch {
                use_id: UseId([1; 16]),
                branch: branch(&format!("refs/heads/{name}"), at),
            },
            at,
            cumulative,
            part,
            false,
        )
    }

    #[test]
    fn brief_usage_rows_put_the_branch_under_review_first() {
        let strings = Strings::of(Locale::En);
        let usage = usage(vec![
            on_branch("a", "/one", 1_000, 100),
            on_branch("b", "/two", 2_000, 500),
            on_branch("c", "/one", 3_000, 300),
        ]);
        let ids = |worktree: Option<&WorktreeKey>| -> Vec<String> {
            usage_lines(&usage, worktree, &strings)
                .into_iter()
                .map(|line| line.id)
                .collect()
        };
        assert_eq!(ids(Some(&worktree("/two"))), ["b", "a", "c"]);
        assert_eq!(ids(Some(&worktree("/one"))), ["a", "c", "b"]);
        assert_eq!(ids(None), ["a", "b", "c"], "the order usage gave");
    }

    #[test]
    fn brief_usage_rows_show_tokens_and_the_sessions_share() {
        let strings = Strings::of(Locale::En);
        let usage = usage(vec![
            on_branch("feat", "/one", 12_345, 3_086),
            row(
                RowKey::Unassigned {
                    unattributable: Tokens::default(),
                },
                "/one",
                0,
                0,
                false,
            ),
        ]);
        let lines = usage_lines(&usage, None, &strings);
        assert_eq!(lines[0].tokens, 12_345);
        assert_eq!(lines[0].percent, 24, "3086 of 12345, rounded down");
        assert_eq!(
            plain(&lines[0].text),
            "feat \u{b7} 12.3K tokens \u{b7} 24% this session"
        );
        assert!(lines[0].shared.is_none());
        assert_eq!(lines[1].id, "Not on any branch");
        assert_eq!(lines[1].percent, 0, "no tokens, no share");
        let zh = usage_lines(&usage, None, &Strings::of(Locale::ZhCn));
        assert!(plain(&zh[0].text).starts_with("feat \u{b7} 12.3K token"));
        assert!(plain(&zh[0].text).ends_with("24%"));
    }

    #[test]
    fn brief_usage_rows_name_mrs_by_title_and_say_when_shared() {
        let strings = Strings::of(Locale::En);
        let mr = |title: &str, number: u64| MrLabel {
            mr: MrRef {
                repo: ForgeRepo {
                    host: Arc::from("example.test"),
                    id: Arc::from("1"),
                },
                number,
            },
            title: Arc::from(title),
            state: MrState::Open,
        };
        let usage = usage(vec![row(
            RowKey::Mrs {
                mrs: Arc::from(vec![mr("Fix the parser", 1), mr("Backport", 2)]),
                branch: branch("refs/heads/feat", "/one"),
            },
            "/one",
            100,
            50,
            true,
        )]);
        let lines = usage_lines(&usage, None, &strings);
        assert_eq!(lines[0].id, "Fix the parser");
        let shared = lines[0].shared.as_ref().expect("shared");
        assert_eq!(plain(shared), "shared with 1 other MR");
        assert_eq!(lines[0].percent, 50);
    }

    #[test]
    fn brief_turn_tokens_are_compact() {
        let strings = Strings::of(Locale::En);
        assert_eq!(plain(&turn_tokens_text(&strings, 950)), "950 tokens");
        assert_eq!(plain(&turn_tokens_text(&strings, 12_345)), "12.3K tokens");
    }
}
