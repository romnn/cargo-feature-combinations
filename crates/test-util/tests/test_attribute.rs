//! Behaviour of `#[test_util::test]` as seen from a test binary.

use color_eyre::eyre;

/// A second install fails only once a handler exists, so this proves the attribute installed one.
#[test_util::test]
fn installs_the_report_handler() {
    assert!(color_eyre::install().is_err());
}

#[test_util::test]
fn propagates_errors_with_question_mark() -> eyre::Result<()> {
    let parsed: u8 = "7".parse()?;
    assert_eq!(parsed, 7);
    Ok(())
}

#[test_util::test]
#[should_panic(expected = "still panics")]
fn keeps_should_panic() {
    panic!("still panics");
}
