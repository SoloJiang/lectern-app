//! The build-time checks (docs/design/i18n.md「编译期校验」), run against one
//! broken resource set per rule in tests/fixtures/. Each set's errors are an
//! insta snapshot; build.rs prints the same messages and fails the build.

#[allow(
    dead_code,
    reason = "build.rs uses the parts of the compiler these tests do not"
)]
#[path = "../build/compile.rs"]
mod compile;

use std::path::Path;

use compile::{Node, VarType, VariantKey};

fn fixture(case: &str) -> Result<compile::Compiled, Vec<compile::Error>> {
    compile::compile_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(case),
    )
}

fn errors(case: &str) -> String {
    match fixture(case) {
        Ok(_) => panic!("{case}: expected errors"),
        Err(errors) => errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[test]
fn real_locales_compile() {
    let compiled = compile::compile_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("locales"))
        .unwrap_or_else(|e| {
            panic!(
                "{}",
                e.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    assert!(compiled.modules.iter().any(|m| m.ident == "common"));
}

#[test]
fn valid_fixture_compiles() {
    let compiled = fixture("valid").unwrap_or_else(|e| panic!("{e:#?}"));
    let entry = |name: &str| {
        compiled
            .entries
            .iter()
            .position(|e| e.name() == name)
            .unwrap()
    };

    // Keys come from en; attributes and attribute-only messages get their own slots.
    let names: Vec<String> = compiled.entries.iter().map(|e| e.name()).collect();
    insta::assert_debug_snapshot!("valid_slots", names);
    let consts: Vec<&str> = compiled
        .entries
        .iter()
        .map(|e| e.const_name.as_str())
        .collect();
    insta::assert_debug_snapshot!("valid_consts", consts);

    // Types: plural selector in en, plain in zh-CN → number; string keys → string;
    // only interpolated → either.
    let nested = &compiled.entries[entry("app-nested")];
    assert_eq!(
        nested.vars,
        [
            ("count".to_owned(), VarType::Num),
            ("kind".to_owned(), VarType::Str),
            ("where".to_owned(), VarType::Any)
        ]
    );
    assert_eq!(
        compiled.entries[entry("app-default-only")].vars,
        [("n".to_owned(), VarType::Any)]
    );

    // References, terms and literals are expanded at build time.
    let en = &compiled.tables[0];
    assert_eq!(en[entry("app-title")], [Node::Text("Lectern".to_owned())]);
    assert_eq!(en[entry("app-literal")], [Node::Text("{1.50A".to_owned())]);
    assert_eq!(
        en[entry("app-ref")],
        [
            Node::Text("Lectern: Say hello to ".to_owned()),
            Node::Var(0)
        ]
    );
    let Node::Select { arms, default, .. } = &en[entry("app-files")][0] else {
        panic!()
    };
    assert_eq!(*default, 2);
    let keys: Vec<&VariantKey> = arms.iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        [
            &VariantKey::Exact(0.0),
            &VariantKey::Plural("one"),
            &VariantKey::Plural("other")
        ]
    );
    let Node::Select { arms, .. } = &en[entry("app-kind")][0] else {
        panic!()
    };
    assert_eq!(arms[2].0, VariantKey::Str("other".to_owned()));
}

#[test]
fn file_sets_must_match() {
    insta::assert_snapshot!(errors("file-sets-differ"));
}

#[test]
fn message_sets_must_match() {
    insta::assert_snapshot!(errors("message-sets-differ"));
}

#[test]
fn attribute_sets_must_match() {
    insta::assert_snapshot!(errors("attribute-sets-differ"));
}

#[test]
fn value_presence_must_match() {
    insta::assert_snapshot!(errors("value-presence-differs"));
}

#[test]
fn variable_sets_must_match() {
    insta::assert_snapshot!(errors("variable-sets-differ"));
}

#[test]
fn selector_variables_count() {
    insta::assert_snapshot!(errors("selector-variables-count"));
}

#[test]
fn variable_types_must_match() {
    insta::assert_snapshot!(errors("variable-types-differ"));
}

#[test]
fn variable_type_conflict_in_one_locale() {
    insta::assert_snapshot!(errors("variable-type-conflict-in-one-locale"));
}

#[test]
fn syntax_errors() {
    insta::assert_snapshot!(errors("syntax-error"));
}

#[test]
fn select_needs_a_default_variant() {
    insta::assert_snapshot!(errors("missing-default-variant"));
}

#[test]
fn ids_are_unique() {
    insta::assert_snapshot!(errors("duplicate-ids"));
}

#[test]
fn ids_start_with_the_file_name() {
    insta::assert_snapshot!(errors("prefix-mismatch"));
}

#[test]
fn references_resolve() {
    insta::assert_snapshot!(errors("unresolved-references"));
}

#[test]
fn references_are_not_circular() {
    insta::assert_snapshot!(errors("circular-references"));
}

#[test]
fn only_the_supported_subset() {
    insta::assert_snapshot!(errors("unsupported-syntax"));
}

#[test]
fn plural_categories_belong_to_the_locale() {
    insta::assert_snapshot!(errors("plural-category-not-in-locale"));
}

#[test]
fn key_constants_are_unique() {
    insta::assert_snapshot!(errors("key-constant-collision"));
}

#[test]
fn variable_count_is_bounded() {
    insta::assert_snapshot!(errors("too-many-variables"));
}

#[test]
fn locale_directories() {
    insta::assert_snapshot!(errors("locale-directories"));
}
