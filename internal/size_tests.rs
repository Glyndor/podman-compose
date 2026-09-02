use super::*;

// parse_memory

#[test]
fn memory_bare_bytes() {
	assert_eq!(parse_memory("1024"), Some(1024));
}

#[test]
fn memory_suffix_b() {
	assert_eq!(parse_memory("512b"), Some(512));
}

#[test]
fn memory_suffix_k() {
	assert_eq!(parse_memory("4k"), Some(4 * 1024));
}

#[test]
fn memory_suffix_kb() {
	assert_eq!(parse_memory("4kb"), Some(4 * 1024));
}

#[test]
fn memory_suffix_m() {
	assert_eq!(parse_memory("128m"), Some(128 * 1024 * 1024));
}

#[test]
fn memory_suffix_mb() {
	assert_eq!(parse_memory("128mb"), Some(128 * 1024 * 1024));
}

#[test]
fn memory_suffix_g() {
	assert_eq!(parse_memory("1g"), Some(1024 * 1024 * 1024));
}

#[test]
fn memory_suffix_uppercase() {
	assert_eq!(parse_memory("64M"), Some(64 * 1024 * 1024));
}

#[test]
fn memory_minus_one_special() {
	assert_eq!(parse_memory("-1"), Some(-1));
}

#[test]
fn memory_empty_is_none() {
	assert_eq!(parse_memory(""), None);
}

#[test]
fn memory_invalid_is_none() {
	assert_eq!(parse_memory("abc"), None);
}

#[test]
fn memory_unknown_suffix_is_none() {
	assert_eq!(parse_memory("100x"), None);
}

#[test]
fn memory_negative_is_none() {
	// A negative memory size is rejected rather than wrapping.
	assert_eq!(parse_memory("-1g"), None);
}

// parse_cpus

#[test]
fn cpus_integer() {
	assert_eq!(parse_cpus("2"), Some(2_000_000_000));
}

#[test]
fn cpus_fraction() {
	assert_eq!(parse_cpus("0.5"), Some(500_000_000));
}

#[test]
fn cpus_empty_is_none() {
	assert_eq!(parse_cpus(""), None);
}

#[test]
fn cpus_non_finite_is_none() {
	assert_eq!(parse_cpus("nan"), None);
	assert_eq!(parse_cpus("inf"), None);
	assert_eq!(parse_cpus("1e300"), None);
}

#[test]
fn cpus_negative_is_none() {
	assert_eq!(parse_cpus("-1"), None);
}

// parse_duration_secs

#[test]
fn duration_secs_plain_s() {
	assert_eq!(parse_duration_secs("30s"), Some(30));
}

#[test]
fn duration_secs_minutes() {
	assert_eq!(parse_duration_secs("2m"), Some(120));
}

#[test]
fn duration_secs_hours() {
	assert_eq!(parse_duration_secs("1h"), Some(3600));
}

#[test]
fn duration_secs_milliseconds_truncates() {
	assert_eq!(parse_duration_secs("500ms"), Some(0));
}

#[test]
fn duration_secs_bare_number() {
	assert_eq!(parse_duration_secs("10"), Some(10));
}

#[test]
fn duration_secs_empty_is_none() {
	assert_eq!(parse_duration_secs(""), None);
}

#[test]
fn duration_secs_unknown_suffix_is_none() {
	assert_eq!(parse_duration_secs("5d"), None);
}

#[test]
fn duration_secs_out_of_range_is_none() {
	// A value whose seconds product exceeds u64::MAX must return None rather
	// than saturating the cast to u64::MAX.
	assert_eq!(parse_duration_secs("1e24"), None);
	assert_eq!(parse_duration_secs("1e24h"), None);
}

// parse_duration_nanos

#[test]
fn duration_nanos_seconds() {
	assert_eq!(parse_duration_nanos("1s"), Some(1_000_000_000));
}

#[test]
fn duration_nanos_milliseconds() {
	assert_eq!(parse_duration_nanos("200ms"), Some(200_000_000));
}

#[test]
fn duration_nanos_minutes() {
	assert_eq!(parse_duration_nanos("1m"), Some(60 * 1_000_000_000));
}

#[test]
fn duration_nanos_nanoseconds() {
	assert_eq!(parse_duration_nanos("500ns"), Some(500));
}

#[test]
fn duration_nanos_overflow_is_none() {
	// A large finite value whose nanosecond product exceeds i64::MAX must
	// return None rather than saturating to i64::MAX.
	assert_eq!(parse_duration_nanos("99999999999h"), None);
}

#[test]
fn duration_nanos_micros_and_hours() {
	assert_eq!(parse_duration_nanos("3us"), Some(3_000));
	assert_eq!(parse_duration_nanos("2h"), Some(2 * 3600 * 1_000_000_000));
}

#[test]
fn duration_nanos_unknown_suffix_is_none() {
	assert_eq!(parse_duration_nanos("5days"), None);
}
