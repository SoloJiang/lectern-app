//! Key misuse does not compile: a `FmtKey` passed to `get`, a `Key` passed to
//! `format`, a mistyped key, or a key built outside the crate.

#[test]
fn key_misuse_does_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
