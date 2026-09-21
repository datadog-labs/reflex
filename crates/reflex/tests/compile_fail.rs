#[test]
fn invalid_declarations_fail_at_compile_time() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/*.rs");
}
