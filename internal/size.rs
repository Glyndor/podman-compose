//! Memory and CPU value parsers shared by the engine and tests.

/// Parse a memory string like `128m`, `1G`, `1024k`, `512b`, or a bare
/// number-of-bytes.
///
/// Recognised suffixes (case-insensitive): `b`, `k`/`kb`, `m`/`mb`,
/// `g`/`gb`, `t`/`tb`.  Returns `None` for unparseable values, and `Some(-1)`
/// for the special string `"-1"` (commonly used to disable swap limits).
pub fn parse_memory(s: &str) -> Option<i64> {
	let trimmed = s.trim();
	if trimmed.is_empty() {
		return None;
	}
	if trimmed == "-1" {
		return Some(-1);
	}

	// Find where the numeric portion ends.
	let split_at = trimmed
		.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
		.unwrap_or(trimmed.len());

	let (num_part, suffix) = trimmed.split_at(split_at);
	let num_part = num_part.trim();
	let suffix = suffix.trim().to_ascii_lowercase();

	let num: f64 = num_part.parse().ok()?;
	if num < 0.0 {
		return None;
	}

	let multiplier: u64 = match suffix.as_str() {
		"" | "b" => 1,
		"k" | "kb" => 1024,
		"m" | "mb" => 1024 * 1024,
		"g" | "gb" => 1024 * 1024 * 1024,
		"t" | "tb" => 1024_u64 * 1024 * 1024 * 1024,
		_ => return None,
	};

	let bytes = num * multiplier as f64;
	if bytes > i64::MAX as f64 {
		return None;
	}
	Some(bytes as i64)
}

/// Parse a CPU count string like `"0.5"`, `"1"`, `"2.5"` into nano-CPUs
/// (1 CPU = 1_000_000_000 nano-CPUs).
///
/// Returns `None` for non-finite (`NaN`/`inf`), negative, or out-of-`i64`-range
/// values instead of silently saturating or wrapping the cast.
pub fn parse_cpus(s: &str) -> Option<i64> {
	let nanos = s.trim().parse::<f64>().ok()? * 1e9;
	if !nanos.is_finite() || nanos < 0.0 || nanos > i64::MAX as f64 {
		return None;
	}
	Some(nanos as i64)
}

/// Parse a duration like `5s`, `200ms`, `1m`, `1h` into seconds (rounded down).
///
/// This is a best-effort parser used when the engine needs an integer
/// seconds value (e.g. Docker API `start_period`).
pub fn parse_duration_secs(s: &str) -> Option<u64> {
	let trimmed = s.trim();
	if trimmed.is_empty() {
		return None;
	}
	let split_at = trimmed
		.find(|c: char| !(c.is_ascii_digit() || c == '.'))
		.unwrap_or(trimmed.len());
	let (num_part, suffix) = trimmed.split_at(split_at);
	let num: f64 = num_part.parse().ok()?;
	let secs = match suffix.trim() {
		"" | "s" => num,
		"ms" => num / 1000.0,
		"us" | "µs" => num / 1_000_000.0,
		"ns" => num / 1_000_000_000.0,
		"m" => num * 60.0,
		"h" => num * 3600.0,
		_ => return None,
	};
	// Reject non-finite or out-of-range results rather than saturating the cast
	// (e.g. `1e24s` would otherwise become u64::MAX and emit a garbage timeout).
	if !secs.is_finite() || secs < 0.0 || secs > u64::MAX as f64 {
		return None;
	}
	Some(secs as u64)
}

/// Parse a duration like `5s`, `200ms`, `1m`, `1h` into nanoseconds (used by
/// Docker healthcheck APIs).
///
/// Returns `None` for non-finite (`NaN`/`inf`), negative, or out-of-`i64`-range
/// results instead of saturating or wrapping the cast.
pub fn parse_duration_nanos(s: &str) -> Option<i64> {
	let trimmed = s.trim();
	if trimmed.is_empty() {
		return None;
	}
	let split_at = trimmed
		.find(|c: char| !(c.is_ascii_digit() || c == '.'))
		.unwrap_or(trimmed.len());
	let (num_part, suffix) = trimmed.split_at(split_at);
	let num: f64 = num_part.parse().ok()?;
	let nanos = match suffix.trim() {
		"" | "s" => num * 1e9,
		"ms" => num * 1e6,
		"us" | "µs" => num * 1e3,
		"ns" => num,
		"m" => num * 60.0 * 1e9,
		"h" => num * 3600.0 * 1e9,
		_ => return None,
	};
	// Reject non-finite or out-of-range results rather than saturating the cast.
	if !nanos.is_finite() || nanos < 0.0 || nanos > i64::MAX as f64 {
		return None;
	}
	Some(nanos as i64)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "size_tests.rs"]
mod tests;
