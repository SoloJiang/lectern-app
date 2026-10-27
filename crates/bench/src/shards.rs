//! The shards: which budgets run in which CI job (docs/design/bench.md
//! 「分片」, ADR 0053). The table is data; it is rearranged by the report's
//! per-benchmark times without a new ADR.

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

/// The shards in the order [`shard_of`] tries them. Every `process` budget
/// is in one shard: the plugin and language server lists the merge expands
/// `plugin/*` and `lsp/*` over come from that target's one report.
pub static SHARDS: &[Shard] = &[
    // The 100,000-file fixture is generated only here.
    Shard {
        name: "code-intel",
        targets: &[Target::Time, Target::Memory],
        prefixes: &["code_intel/", "plugin/code-intel/"],
    },
    Shard {
        name: "process",
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

    #[test]
    fn shards_process_budgets_are_in_one_shard() {
        let names: std::collections::BTreeSet<&str> = BUDGETS
            .iter()
            .filter(|b| b.target == Target::Process)
            .map(|b| shard_of(b).unwrap().name)
            .collect();
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["process"]);
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
