//! Argument value parsers for the CLI.

/// Parse a `SERVICE=N` scale argument into a `(service, replicas)` pair.
///
/// Rejects a missing `=`, an empty service name, a non-numeric count, and `N=0`
/// (use `down`/`stop` to remove a service, not `scale=0`). The count must be a
/// run of plain ASCII digits: a leading sign such as `+3` (which `u32::FromStr`
/// would otherwise accept) is rejected so the input contract stays consistent
/// with the already-rejected `-1`/`x`/`0x10` forms.
pub(crate) fn parse_scale_pair(value: &str) -> Result<(String, u32), String> {
	let (service, count) = value
		.split_once('=')
		.ok_or_else(|| format!("expected SERVICE=N, got `{value}`"))?;
	if service.is_empty() {
		return Err(format!("missing service name in `{value}`"));
	}
	if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
		return Err(format!(
			"replica count in `{value}` must be a non-negative integer"
		));
	}
	let replicas: u32 = count
		.parse()
		.map_err(|_| format!("replica count in `{value}` must be a non-negative integer"))?;
	if replicas == 0 {
		return Err(format!(
			"replica count in `{value}` must be at least 1; use `down`/`stop` to remove a service"
		));
	}
	Ok((service.to_string(), replicas))
}

/// Pull-policy values podup accepts for `up --pull` / `pull --policy`. `always`,
/// `missing`, `never`, and `build` are the Compose Spec policies; `newer` is Podman's
/// extension.
const PULL_POLICIES: [&str; 5] = ["always", "missing", "never", "newer", "build"];

/// Validate a `--pull` / `--policy` value at parse time, rejecting unknown values
/// with a clear message instead of silently defaulting to `missing` at runtime.
pub(crate) fn parse_pull_policy(value: &str) -> Result<String, String> {
	if PULL_POLICIES.contains(&value) {
		Ok(value.to_string())
	} else {
		Err(format!(
			"invalid pull policy `{value}` (expected one of: {})",
			PULL_POLICIES.join(", ")
		))
	}
}

/// The progress styles the Compose Spec defines. podup renders build
/// output one way, so the flag is inert, but an unknown value must still be
/// rejected rather than accepted and ignored, or a typo silently changes nothing
/// and reports success.
const PROGRESS_STYLES: [&str; 3] = ["auto", "plain", "tty"];

/// Validate a `build --progress` value at parse time.
pub(crate) fn parse_progress(value: &str) -> Result<String, String> {
	if PROGRESS_STYLES.contains(&value) {
		Ok(value.to_string())
	} else {
		Err(format!(
			"invalid progress style `{value}` (expected one of: {})",
			PROGRESS_STYLES.join(", ")
		))
	}
}

/// Parse a `-t/--timeout` shutdown-grace value, rejecting negatives with a clear
/// range error rather than forwarding `-5` to Podman or letting clap report a
/// confusing "unexpected argument" for the space form.
pub(crate) fn parse_timeout(value: &str) -> Result<i32, String> {
	let secs: i32 = value
		.parse()
		.map_err(|_| format!("timeout `{value}` must be an integer number of seconds"))?;
	if secs < 0 {
		return Err(format!("timeout `{value}` must be zero or greater"));
	}
	Ok(secs)
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
