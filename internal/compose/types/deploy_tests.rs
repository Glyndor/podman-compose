use super::CountOrAll;

#[test]
fn count_or_all_named_returns_minus_one() {
	assert_eq!(CountOrAll::Named("all".into()).to_i64(), -1);
}

#[test]
fn count_or_all_n_returns_value() {
	assert_eq!(CountOrAll::N(4).to_i64(), 4);
}
