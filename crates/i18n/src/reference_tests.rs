//! The generated formatter against fluent-bundle, the reference implementation
//! (docs/design/i18n.md「测试」).
//!
//! Every slot of every language is formatted with synthetic arguments by both
//! and must give the same text once the directional isolates are removed;
//! fluent-bundle must report no error.
//!
//! Arguments: every number variable takes 0, 1, 2, 5, 1.5 and every number
//! literal used as a key for it; every string variable takes a fixed text and
//! every string key used for it; a variable that is only interpolated takes
//! both sets. All combinations are tried. This reaches every reachable branch
//! path: each key's value is a candidate, and a value matching no key (1.5,
//! never a key because decimal keys are rejected at build time, or the fixed
//! text) reaches each default, so every combination of choices along a nested
//! path is among the combinations.

use std::fs;
use std::path::Path;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};

use crate::args::Args;
use crate::table::{self, Part, Selector, VarType};
use crate::{Locale, Strings};

const NUMBERS: [f64; 5] = [0.0, 1.0, 2.0, 5.0, 1.5];
const TEXT: &str = "Fixture text";

fn bundle(locale: Locale) -> FluentBundle<FluentResource> {
    let mut bundle = FluentBundle::new(vec![locale.tag().parse().unwrap()]);
    // Isolation is compared separately; the generated code isolates only strings.
    bundle.set_use_isolating(false);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("locales")
        .join(locale.tag());
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for file in files
        .into_iter()
        .filter(|f| f.extension().is_some_and(|e| e == "ftl"))
    {
        let resource = FluentResource::try_new(fs::read_to_string(&file).unwrap())
            .unwrap_or_else(|(_, e)| panic!("{}: {e:?}", file.display()));
        bundle
            .add_resource(resource)
            .unwrap_or_else(|e| panic!("{}: {e:?}", file.display()));
    }
    bundle
}

/// Number and string keys used for variable `var` anywhere in `parts`.
fn literals(parts: &[Part], var: u8, numbers: &mut Vec<f64>, strings: &mut Vec<String>) {
    for part in parts {
        if let Part::Select { var: v, arms, .. } = part {
            for (selector, arm) in *arms {
                if *v == var {
                    match selector {
                        Selector::Exact(n) if !numbers.contains(n) => numbers.push(*n),
                        Selector::Str(s) => strings.push((*s).to_owned()),
                        Selector::Plural(category) => strings.push(category.name().to_owned()),
                        Selector::Exact(_) => {}
                    }
                }
                literals(arm, var, numbers, strings);
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Candidate {
    Num(f64),
    Str(String),
}

fn candidates(index: usize) -> Vec<Vec<Candidate>> {
    table::VARS[index]
        .iter()
        .enumerate()
        .map(|(var, &(_, ty))| {
            let var = u8::try_from(var).unwrap();
            let mut numbers = NUMBERS.to_vec();
            let mut strings = vec![TEXT.to_owned()];
            for table in [&table::EN, &table::ZH_CN] {
                literals(table[index], var, &mut numbers, &mut strings);
            }
            strings.dedup();
            let numbers = numbers.into_iter().map(Candidate::Num);
            let strings = strings.into_iter().map(Candidate::Str);
            match ty {
                VarType::Num => numbers.collect(),
                VarType::Str => strings.collect(),
                VarType::Any => numbers.chain(strings).collect(),
            }
        })
        .collect()
}

fn combinations(candidates: &[Vec<Candidate>]) -> Vec<Vec<Candidate>> {
    candidates.iter().fold(vec![Vec::new()], |combos, choices| {
        combos
            .iter()
            .flat_map(|combo| {
                choices.iter().map(move |choice| {
                    let mut next = combo.clone();
                    next.push(choice.clone());
                    next
                })
            })
            .collect()
    })
}

#[test]
fn generated_formatter_matches_fluent_bundle() {
    let mut checked = 0;
    for locale in Locale::ALL {
        let bundle = bundle(locale);
        let strings = Strings::of(locale);
        for (index, name) in table::NAMES.iter().enumerate() {
            let (id, attribute) = match name.split_once('.') {
                Some((id, attr)) => (id, Some(attr)),
                None => (*name, None),
            };
            let message = bundle
                .get_message(id)
                .unwrap_or_else(|| panic!("{name}: not in fluent-bundle"));
            let pattern = match attribute {
                Some(attr) => message.get_attribute(attr).unwrap().value(),
                None => message.value().unwrap(),
            };
            let vars = table::VARS[index];
            for combo in combinations(&candidates(index)) {
                let mut args = Args::new();
                let mut fluent_args = FluentArgs::new();
                for (&(var, _), candidate) in vars.iter().zip(&combo) {
                    match candidate {
                        Candidate::Num(n) => {
                            args = args.num(var, *n);
                            fluent_args.set(var, FluentValue::from(*n));
                        }
                        Candidate::Str(s) => {
                            args = args.str(var, s);
                            fluent_args.set(var, FluentValue::from(s.as_str()));
                        }
                    }
                }
                let mut errors = Vec::new();
                let expected = bundle.format_pattern(pattern, Some(&fluent_args), &mut errors);
                assert!(
                    errors.is_empty(),
                    "{} {name} {combo:?}: fluent-bundle errors {errors:?}",
                    locale.tag()
                );
                let (actual, error) = strings.format_checked(index, &args);
                assert_eq!(error, None, "{} {name} {combo:?}", locale.tag());
                let actual = actual.replace(['\u{2068}', '\u{2069}'], "");
                assert_eq!(actual, expected, "{} {name} {combo:?}", locale.tag());
                checked += 1;
            }
        }
    }
    assert!(checked > table::LEN, "checked {checked} combinations");
}

#[test]
fn every_message_and_attribute_has_a_slot() {
    for locale in Locale::ALL {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("locales")
            .join(locale.tag());
        let mut names = Vec::new();
        for file in fs::read_dir(&dir).unwrap() {
            let text = fs::read_to_string(file.unwrap().path()).unwrap();
            let resource = fluent_syntax::parser::parse(text.as_str()).unwrap();
            for entry in resource.body {
                if let fluent_syntax::ast::Entry::Message(m) = entry {
                    if m.value.is_some() {
                        names.push(m.id.name.to_owned());
                    }
                    names.extend(
                        m.attributes
                            .iter()
                            .map(|a| format!("{}.{}", m.id.name, a.id.name)),
                    );
                }
            }
        }
        names.sort();
        let mut slots: Vec<String> = table::NAMES.iter().map(|n| (*n).to_owned()).collect();
        slots.sort();
        assert_eq!(names, slots, "{}", locale.tag());
    }
}

#[test]
fn slots_with_variables_are_fmt_keys() {
    // A Key slot has no variables and compiles to one Text; a FmtKey slot has
    // variables. The types themselves are checked by tests/compile_fail.rs.
    for (index, vars) in table::VARS.iter().enumerate() {
        for table in [&table::EN, &table::ZH_CN] {
            let plain = matches!(table[index], [Part::Text(_)]);
            if vars.is_empty() {
                assert!(
                    plain,
                    "{}: a Key slot must be one Text",
                    table::NAMES[index]
                );
            }
        }
    }
}
