//! The shards: which budgets run in which CI job (docs/design/bench.md
//! 「分片」, ADR 0053, ADR 0062). The table is data; it is rearranged by the
//! report's per-benchmark times without a new ADR.

use crate::budgets::{Budget, Target};

/// One shard: a set of benchmark targets and, in them, the budgets it owns.
#[derive(Debug)]
pub struct Shard {
    pub name: &'static str,
    /// The shard's benchmark targets; `just bench --shard` builds and runs
    /// only these.
    pub targets: &'static [Target],
    /// The ID prefixes in those targets that belong to this shard; empty
    /// means the rest of the budgets of those targets.
    pub prefixes: &'static [&'static str],
}

/// The shards in the order [`shard_of`] tries them. The `process` target is
/// in four shards (ADR 0062), but every `plugin/` and `lsp/` budget is in
/// the shard called `process`: the plugin and language server lists the
/// merge expands `plugin/*` and `lsp/*` over come from that shard's report.
/// `process-app` has no prefix, so it is the last of the four: it takes
/// the budgets the other two do not name, such as `app/*` budgets added
/// later.
pub static SHARDS: &[Shard] = &[
    // The 100,000-file fixture is generated only here.
    Shard {
        name: "code-intel",
        targets: &[Target::Time, Target::Memory],
        prefixes: &["code_intel/", "plugin/code-intel/"],
    },
    // The lists' shard: the plugin benchmark (about 280 s), the empty window
    // baseline and the cold start and idle benchmarks.
    Shard {
        name: "process",
        targets: &[Target::Process],
        prefixes: &[
            "plugin/",
            "lsp/",
            "baseline/",
            "app/cold_start",
            "app/idle_",
            "app/unclassified_children",
        ],
    },
    // One benchmark of its own: its scenarios are launches in one function.
    Shard {
        name: "process-open-file",
        targets: &[Target::Process],
        prefixes: &["app/open_file"],
    },
    // The scroll frame benchmarks (about 465 s on the hosted runner), with
    // `app/enter_edit` and `app/open_project` to even out the two shards
    // (`process-app` alone took 20.5 minutes, ADR 0062).
    Shard {
        name: "process-frames",
        targets: &[Target::Process],
        prefixes: &["app/scroll_frames", "app/enter_edit", "app/open_project"],
    },
    // The rest of the `process` target: the open and navigation benchmarks.
    Shard {
        name: "process-app",
        targets: &[Target::Process],
        prefixes: &[],
    },
    Shard {
        name: "git",
        targets: &[Target::Time, Target::Memory],
        prefixes: &["git/"],
    },
    Shard {
        name: "rest",
        targets: &[Target::Time, Target::Memory],
        prefixes: &[],
    },
];

/// The shard of `budget` (before `*` is expanded): the first shard, in
/// table order, whose `targets` has `budget.target` and whose `prefixes` is
/// empty or has a prefix of `budget.id`. `Package` budgets, which the
/// packaging job measures, belong to none.
pub fn shard_of(budget: &Budget) -> Option<&'static Shard> {
    shard_in(SHARDS, budget)
}

/// [`shard_of`] over another table.
pub(crate) fn shard_in<'s>(shards: &'s [Shard], budget: &Budget) -> Option<&'s Shard> {
    shards.iter().find(|shard| {
        shard.targets.contains(&budget.target)
            && (shard.prefixes.is_empty()
                || shard.prefixes.iter().any(|p| budget.id.starts_with(p)))
    })
}

/// The shard called `name`.
pub fn by_name(name: &str) -> Option<&'static Shard> {
    SHARDS.iter().find(|shard| shard.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budgets::{BUDGETS, State};

    #[test]
    fn shards_every_benchmark_budget_is_in_exactly_one_shard() {
        for budget in BUDGETS.iter().filter(|b| b.target != Target::Package) {
            // `state` is Active or Planned: both are in the report.
            assert!(matches!(budget.state, State::Active | State::Planned(_)));
            // The first match in table order is the shard: a budget with
            // a prefix is not left to the shard that takes the rest.
            let shard = shard_of(budget).unwrap_or_else(|| panic!("{} has no shard", budget.id));
            assert!(shard.targets.contains(&budget.target), "{}", budget.id);
            let first = SHARDS
                .iter()
                .position(|s| shard_in(std::slice::from_ref(s), budget).is_some())
                .unwrap();
            assert_eq!(SHARDS[first].name, shard.name, "{}", budget.id);
        }
    }

    #[test]
    fn shards_package_budgets_are_in_none() {
        let package: Vec<&Budget> = BUDGETS
            .iter()
            .filter(|b| b.target == Target::Package)
            .collect();
        assert!(!package.is_empty());
        for budget in package {
            assert!(shard_of(budget).is_none(), "{}", budget.id);
        }
    }

    /// The lists the merge expands `plugin/*` and `lsp/*` over come from one
    /// report: every budget that a list's benchmark produces or expands over
    /// is in one shard (ADR 0062).
    #[test]
    fn shards_process_list_budgets_are_in_the_process_shard() {
        let mut found = 0;
        for budget in BUDGETS.iter().filter(|b| b.target == Target::Process) {
            if budget.id.starts_with("plugin/") || budget.id.starts_with("lsp/") {
                found += 1;
                assert_eq!(shard_of(budget).unwrap().name, "process", "{}", budget.id);
            }
            if budget.is_wildcard() {
                assert_eq!(shard_of(budget).unwrap().name, "process", "{}", budget.id);
            }
        }
        assert!(found > 0);
    }

    /// The `process` target is split, and every shard of it has a budget to
    /// run.
    #[test]
    fn shards_the_process_target_is_in_several_shards() {
        let mut names = std::collections::BTreeMap::<&str, usize>::new();
        for budget in BUDGETS.iter().filter(|b| b.target == Target::Process) {
            *names.entry(shard_of(budget).unwrap().name).or_default() += 1;
        }
        let process: Vec<&str> = SHARDS
            .iter()
            .filter(|s| s.targets.contains(&Target::Process))
            .map(|s| s.name)
            .collect();
        assert!(process.len() > 1);
        for name in process {
            assert!(names.contains_key(name), "{name} has no process budget");
        }
    }

    /// One benchmark's budgets are not split between shards: the benchmark
    /// `app/open_file` runs six scenarios in one function, and `plugin`
    /// produces every `plugin/*/…` result, so a prefix must name them all.
    #[test]
    fn shards_a_benchmarks_budgets_stay_in_one_shard() {
        let owner = |id: &str| {
            shard_of(crate::budgets::lookup(BUDGETS, id).unwrap())
                .unwrap()
                .name
        };
        for (id, shard) in [
            ("app/open_file", "process-open-file"),
            ("app/nav_back", "process-app"),
            ("app/files_open", "process-app"),
            ("app/files_expand", "process-app"),
            ("app/open_project", "process-frames"),
            ("app/enter_edit", "process-frames"),
            ("app/scroll_frames", "process-frames"),
            ("app/scroll_frames_indexing", "process-frames"),
            ("app/scroll_frames/main_thread/one_line", "process-frames"),
            ("app/scroll_frames_indexing/main_thread", "process-frames"),
            ("app/switch_worktree", "process-app"),
            ("app/cold_start", "process"),
            ("app/cold_start_purged", "process"),
            ("app/idle_100k", "process"),
            ("app/unclassified_children", "process"),
            ("baseline/empty_window_idle", "process"),
            ("plugin/after_first_frame", "process"),
        ] {
            assert_eq!(owner(id), shard, "{id}");
        }
    }

    /// A budget that no prefix names is not left out: the last shard of a
    /// target with no prefix takes it (`app/open_diff` and the budgets
    /// that later steps add need no table change).
    #[test]
    fn shards_an_unnamed_process_budget_goes_to_the_catch_all() {
        for id in ["app/open_diff", "app/goto_candidates", "app/new_thing"] {
            let budget = Budget {
                id,
                checks: &[],
                tier: crate::budgets::Tier::Top,
                source: "",
                state: State::Active,
                target: Target::Process,
                macos_only: true,
                judging_machine: false,
            };
            assert_eq!(shard_of(&budget).unwrap().name, "process-app", "{id}");
        }
    }

    #[test]
    fn shards_code_intel_resident_budgets_follow_the_fixture() {
        let mut found = 0;
        for budget in BUDGETS
            .iter()
            .filter(|b| b.id.starts_with("plugin/code-intel/resident_"))
        {
            found += 1;
            assert_eq!(budget.target, Target::Memory, "{}", budget.id);
            assert_eq!(
                shard_of(budget).unwrap().name,
                "code-intel",
                "{}",
                budget.id
            );
        }
        assert!(found > 0);
        // The expanded `plugin/*/activate` ID is not looked up: its budget
        // is a `process` one.
        let activate = crate::budgets::lookup(BUDGETS, "plugin/code-intel/activate").unwrap();
        assert_eq!(shard_of(activate).unwrap().name, "process");
        for budget in BUDGETS.iter().filter(|b| b.id.starts_with("code_intel/")) {
            assert_eq!(
                shard_of(budget).unwrap().name,
                "code-intel",
                "{}",
                budget.id
            );
        }
    }

    #[test]
    fn shards_names_are_unique_and_each_has_a_target() {
        let mut names = std::collections::BTreeSet::new();
        for shard in SHARDS {
            assert!(names.insert(shard.name), "{}", shard.name);
            assert!(!shard.targets.is_empty(), "{}", shard.name);
            assert!(!shard.targets.contains(&Target::Package), "{}", shard.name);
            assert_eq!(by_name(shard.name).map(|s| s.name), Some(shard.name));
        }
        assert!(by_name("nope").is_none());
    }
}
