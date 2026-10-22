//! 「测试」隐私: misuse of the macros does not compile
//! (`string_field_does_not_compile`, `format_message_does_not_compile`,
//! `reserved_key_does_not_compile`).

#[test]
fn macro_misuse_does_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}
