use super::{parse, settle};

#[test]
fn parses_grouped_and_typographic_minus_and_refuses_junk() {
    assert_eq!(parse("1,234.5"), Some(1234.5));
    assert_eq!(parse("\u{2212}3"), Some(-3.0));
    assert_eq!(parse("abc"), None);
    assert_eq!(parse("1e999"), None);
}

#[test]
fn settle_clamps_then_rounds() {
    assert_eq!(settle(1.23456, 0.0, 10.0, 2), 1.23);
    assert_eq!(settle(-5.0, 0.0, 10.0, 0), 0.0);
    assert_eq!(settle(12.6, 0.0, 10.0, 0), 10.0);
}

#[test]
fn settle_rounds_before_it_clamps() {
    assert_eq!(settle(0.2 + 0.1, 0.0, 0.25, 1), 0.2);
    assert_eq!(settle(-0.26, -0.25, 1.0, 1), -0.2);
    assert_eq!(settle(0.3, 0.0, 0.29, 2), 0.29);
    assert_eq!(settle(0.29, 0.29, 0.29, 2), 0.29);
    assert_eq!(settle(10000000.01, 0.0, 10000000.005, 2), 10000000.0);
}

#[test]
#[should_panic(expected = "no 1-place number lies in 0.21..=0.29")]
fn settle_refuses_a_range_without_a_number_at_its_precision() {
    settle(0.25, 0.21, 0.29, 1);
}
