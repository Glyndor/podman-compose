//! Pre-validation of fields libpod validates on its own.
//!
//! libpod validates a handful of fields when a `SpecGenerator` arrives over
//! the create endpoint and when a build query string arrives at the build
//! endpoint. The validators are scattered across the libpod source — namespaces
//! via `ParseNamespace`, `device_cgroup_rule` access strings via
//! `parseLinuxResourcesDeviceAccess`, and a handful of others (build-arg keys,
//! label keys) rejected by the buildkit-fronted parser. When libpod rejects
//! one of these, the raw error message names the validator, not the
//! compose-side field. Podup owns the compose-side translation, so it also
//! owns the field-aware error surface: this module runs the same allow-lists
//! libpod uses, produces a structured `PodmanError::Field` before contacting
//! the daemon, and lets every other field fall through to the runtime.
//!
//! `#1357` reframed the original "per-field allow-list for compose fields
//! forwarded to libpod" proposal into this: pre-validate what libpod
//! pre-validates, name the field and value, and never invent allow-lists for
//! the rest.

use std::fmt::Write as _;

use crate::error::ComposeError;

use super::error::PodmanError;

/// Compose-side field names for each `SpecGenerator` namespace slot.
const PID_FIELD: &str = "pid";
const IPC_FIELD: &str = "ipc";
const UTS_FIELD: &str = "uts";
const USERNS_FIELD: &str = "userns_mode";
const CGROUP_FIELD: &str = "cgroup";

/// The namespace modes libpod's `ParseNamespace` accepts (in the form a
/// compose-side string would be in).
///
/// `host`, `private`, `pod`, and `none` are the simple modes. `container:<id>`
/// joins another container's namespace (compose's `container:NAME` and
/// `service:NAME` forms; podup rewrites service→container before this list
/// sees it). The `ns:<path>` form joins a namespace by an absolute filesystem
/// path — directly user-facing on the compose side, so it has to be allowed.
///
/// `network_mode` is intentionally **not** validated here: the engine
/// translates `service:NAME` to `container:<cname>` and accepts `bridge`,
/// which is a libpod netns mode but is not a member of the strict pid/ipc/uts/
/// user/cgroup allow-list. Validating `network_mode` against this list would
/// reject a working compose file. The engine validates the *result* of the
/// translation post-hoc; a rejected value still surfaces through the
/// `netns` field of the rendered error, just via the libpod message rather
/// than the pre-validator.
/// The modes every namespace slot accepts.
const NS_MODES: &[&str] = &["host", "private", "pod"];

/// `ipc` takes two modes the other slots reject. `shareable` is the one
/// compose files actually reach for — it is what lets a second container join
/// this one's IPC namespace later, and podup used to refuse it outright.
const IPC_EXTRA_MODES: &[&str] = &["none", "shareable"];

/// `userns_mode` has its own vocabulary, and it is the reason this list had to
/// stop being shared. `keep-id` maps the calling user into the container and
/// is the standard rootless answer to a file-ownership mismatch; `nomap` maps
/// nothing; `auto` lets podman pick a free range.
const USERNS_EXTRA_MODES: &[&str] = &["keep-id", "auto", "nomap"];

/// `container:` is not a value in the allow-list; it must carry an id.
/// `ns:` is not a value either — it must carry a path. The presence of the
/// prefix is the test; the suffix is whatever the user typed.
const NS_PREFIX_MODES: &[&str] = &["container:", "ns:"];

/// `keep-id` and `auto` also take options, as `keep-id:uid=1000,gid=1000` or
/// `auto:size=65536`. Only `userns_mode` accepts these.
const USERNS_PREFIX_MODES: &[&str] = &["keep-id:", "auto:"];

/// Per-namespace validator entry: the compose-side field name and the mode
/// value to validate. `None` means the namespace slot was unset (skip).
type NsSlot<'a> = (&'a str, Option<&'a str>);

/// Validate the namespace slots a compose service set, against the same
/// allow-list libpod's `ParseNamespace` accepts. Returns the first failing
/// slot as a `(field, value, allowed_modes)` triple so the caller can format
/// a `Field` error, or `None` when every slot is either unset or accepted.
pub(crate) fn first_invalid_namespace(slots: &[NsSlot<'_>]) -> Option<(String, String, String)> {
	for (field, value) in slots {
		let Some(mode) = value else { continue };
		if is_valid_namespace_mode(field, mode) {
			continue;
		}
		return Some((
			(*field).to_string(),
			(*mode).to_string(),
			allowed_namespace_modes(field),
		));
	}
	None
}

/// The plain modes this slot accepts on top of [`NS_MODES`].
fn extra_modes(field: &str) -> &'static [&'static str] {
	match field {
		IPC_FIELD => IPC_EXTRA_MODES,
		USERNS_FIELD => USERNS_EXTRA_MODES,
		_ => &[],
	}
}

/// The `prefix:suffix` forms this slot accepts on top of [`NS_PREFIX_MODES`].
fn extra_prefixes(field: &str) -> &'static [&'static str] {
	if field == USERNS_FIELD {
		USERNS_PREFIX_MODES
	} else {
		&[]
	}
}

fn is_valid_namespace_mode(field: &str, mode: &str) -> bool {
	if NS_MODES.contains(&mode) || extra_modes(field).contains(&mode) {
		return true;
	}
	// Measure the suffix against the prefix that actually matched. The old
	// code compared every prefix against `"container:".len()`, which would
	// have rejected a short but legal `ns:/x`.
	for p in NS_PREFIX_MODES.iter().chain(extra_prefixes(field)) {
		if mode.starts_with(p) {
			return mode.len() > p.len();
		}
	}
	false
}

fn allowed_namespace_modes(field: &str) -> String {
	let mut s = String::from("one of ");
	let mut first = true;
	let mut sep = |s: &mut String, first: &mut bool| {
		if !*first {
			s.push_str(", ");
		}
		*first = false;
	};
	for m in NS_MODES.iter().chain(extra_modes(field)) {
		sep(&mut s, &mut first);
		write!(&mut s, "`{m}`").expect("writing to String never fails");
	}
	for p in NS_PREFIX_MODES.iter().chain(extra_prefixes(field)) {
		sep(&mut s, &mut first);
		write!(&mut s, "`{p}<id-or-path>`").expect("writing to String never fails");
	}
	s
}

// ---------------------------------------------------------------------------
// device_cgroup_rule access validation
// ---------------------------------------------------------------------------

/// Validate a `device_cgroup_rule` access string against libpod's
/// `parseLinuxResourcesDeviceAccess`. The OCI runtime-spec allows any
/// combination of `r`, `w`, `m` (read, write, mknod); a non-empty access
/// string that is not a subset of those three letters is rejected.
pub(crate) fn first_invalid_device_access<'a, I>(rules: I) -> Option<(String, String)>
where
	I: IntoIterator<Item = &'a str>,
{
	for (i, access) in rules.into_iter().enumerate() {
		if access.is_empty() {
			continue;
		}
		if !is_valid_device_access(access) {
			return Some((
				format!("device_cgroup_rule[{i}].access"),
				access.to_string(),
			));
		}
	}
	None
}

fn is_valid_device_access(access: &str) -> bool {
	!access.is_empty()
		&& access.chars().all(|c| matches!(c, 'r' | 'w' | 'm'))
		&& access.chars().any(|c| matches!(c, 'r' | 'w' | 'm'))
}

// ---------------------------------------------------------------------------
// build query key validation
// ---------------------------------------------------------------------------

/// The OCI build-arg / build-label key charset libpod accepts.
///
/// libpod's buildkit-fronted parser rejects keys that contain any character
/// outside `[A-Za-z0-9_.-]`. Validating client-side lets a bad key surface as
/// a `build.args: ... (value: "...")` error naming the field, instead of
/// libpod's `400` body (which names the key but not the compose-side field)
/// (#1357).
fn is_valid_kv_key(key: &str) -> bool {
	!key.is_empty()
		&& key
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Validate every key in a `build.args` / `build.labels` map. Returns the
/// first invalid key as `(field, key, message)` so the caller can build a
/// `Field` error.
pub(crate) fn first_invalid_kv_key<'a, I>(field: &str, keys: I) -> Option<(String, String, String)>
where
	I: IntoIterator<Item = &'a str>,
{
	for key in keys {
		if is_valid_kv_key(key) {
			continue;
		}
		return Some((
			field.to_string(),
			key.to_string(),
			format!("key {key:?} is not a valid identifier; must match `[A-Za-z0-9_.-]+`"),
		));
	}
	None
}

// ---------------------------------------------------------------------------
// value rendering
// ---------------------------------------------------------------------------

/// Truncate a value for inclusion in a `Field` error so a huge or binary value
/// does not flood the rendered message. Multi-line values are collapsed onto
/// one line so the message stays single-line. The threshold is generous
/// (256 chars) — long enough for any realistic compose value, short enough
/// to keep the error readable.
pub(crate) fn render_value(value: &str) -> String {
	let mut s = String::with_capacity(value.len().min(256));
	for (i, c) in value.chars().enumerate() {
		if i >= 256 {
			s.push('…');
			break;
		}
		match c {
			'\n' => s.push_str("\\n"),
			'\r' => s.push_str("\\r"),
			'\t' => s.push_str("\\t"),
			c if c.is_control() => write!(&mut s, "\\u{{{:x}}}", c as u32).unwrap(),
			c => s.push(c),
		}
	}
	s
}

// ---------------------------------------------------------------------------
// build field error
// ---------------------------------------------------------------------------

/// Construct a `PodmanError::Field` for a build-query parameter.
pub(crate) fn build_field_error(
	field: impl Into<String>,
	value: impl Into<String>,
	message: impl Into<String>,
) -> PodmanError {
	let value_str = value.into();
	PodmanError::Field {
		service: String::new(),
		field: field.into(),
		value: render_value(&value_str),
		message: message.into(),
	}
}

/// Construct a `PodmanError::Field` for a `SpecGenerator` field that podup
/// pre-validated.
pub(crate) fn spec_field_error(
	service: impl Into<String>,
	field: impl Into<String>,
	value: impl Into<String>,
	message: impl Into<String>,
) -> PodmanError {
	let value_str = value.into();
	PodmanError::Field {
		service: service.into(),
		field: field.into(),
		value: render_value(&value_str),
		message: message.into(),
	}
}

/// Pre-validate the `SpecGenerator` fields libpod validates on its own, so a
/// rejected value surfaces as a `PodmanError::Field` carrying the compose
/// field name and offending value instead of libpod's raw validator text.
pub(crate) fn pre_validate_spec(
	service_name: &str,
	service: &crate::compose::types::Service,
	device_cgroup_access: &[String],
) -> Result<(), ComposeError> {
	// 1. Namespace modes for the slots whose compose string is forwarded
	//    verbatim to `ParseNamespace`. `network_mode` is omitted: the engine
	//    translates it (`service:NAME` → `container:<cname>`, plus `bridge`
	//    is a valid netns-only mode not in the strict allow-list), so the
	//    validator would reject a working compose file. Its result is checked
	//    post-translation when the spec is built.
	let slots: Vec<(&str, Option<&str>)> = vec![
		(PID_FIELD, service.pid.as_deref()),
		(IPC_FIELD, service.ipc.as_deref()),
		(UTS_FIELD, service.uts.as_deref()),
		(USERNS_FIELD, service.userns_mode.as_deref()),
		(CGROUP_FIELD, service.cgroup.as_deref()),
	];
	if let Some((field, value, allowed)) = first_invalid_namespace(&slots) {
		let msg = format!("namespace mode {value:?} is not recognised; must be {allowed}");
		return Err(ComposeError::Podman(spec_field_error(
			service_name,
			field,
			value,
			msg,
		)));
	}

	// 2. device_cgroup_rule access strings.
	if let Some((field, value)) =
		first_invalid_device_access(device_cgroup_access.iter().map(String::as_str))
	{
		let msg =
			format!("access string {value:?} is not one of `r`, `w`, `m` or a combination thereof");
		return Err(ComposeError::Podman(spec_field_error(
			service_name,
			field,
			value,
			msg,
		)));
	}

	Ok(())
}

/// Pre-validate the build-query fields libpod validates. Called from the
/// build path before the URL is assembled, so a bad key fails before any
/// POST to the daemon (#1357).
pub(crate) fn pre_validate_build(
	build_args: &std::collections::HashMap<String, String>,
	labels: &std::collections::HashMap<String, String>,
) -> Result<(), ComposeError> {
	if let Some((field, key, msg)) =
		first_invalid_kv_key("build.args", build_args.keys().map(String::as_str))
	{
		return Err(ComposeError::Podman(build_field_error(field, key, msg)));
	}
	if let Some((field, key, msg)) =
		first_invalid_kv_key("build.labels", labels.keys().map(String::as_str))
	{
		return Err(ComposeError::Podman(build_field_error(field, key, msg)));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::HashMap;

	/// The modes every slot shares.
	#[test]
	fn namespace_modes_accept_allow_list() {
		for field in [PID_FIELD, IPC_FIELD, UTS_FIELD, USERNS_FIELD, CGROUP_FIELD] {
			assert!(is_valid_namespace_mode(field, "host"), "{field}");
			assert!(is_valid_namespace_mode(field, "private"), "{field}");
			assert!(is_valid_namespace_mode(field, "pod"), "{field}");
			assert!(is_valid_namespace_mode(field, "container:abc"), "{field}");
			assert!(
				is_valid_namespace_mode(field, "ns:/run/netns/foo"),
				"{field}"
			);
		}
	}

	#[test]
	fn namespace_modes_reject_unknown() {
		assert!(!is_valid_namespace_mode(PID_FIELD, "evil"));
		assert!(!is_valid_namespace_mode(PID_FIELD, ""));
		assert!(!is_valid_namespace_mode(PID_FIELD, "HOST"));
		assert!(!is_valid_namespace_mode(PID_FIELD, "container:"));
		assert!(!is_valid_namespace_mode(PID_FIELD, "ns:"));
	}

	/// A one-character path is still a path. The old check measured every
	/// prefix against `"container:".len()`, so `ns:/x` failed for being short
	/// rather than for being wrong.
	#[test]
	fn a_short_ns_path_is_accepted() {
		assert!(is_valid_namespace_mode(PID_FIELD, "ns:/x"));
	}

	/// Measured against podman 5.7.0: `shareable` and `none` parse for `ipc`
	/// and are rejected by every other slot with "unrecognized namespace
	/// mode". `shareable` is the one compose files reach for, and podup used
	/// to refuse it.
	#[test]
	fn ipc_takes_shareable_and_none_but_no_other_slot_does() {
		assert!(is_valid_namespace_mode(IPC_FIELD, "shareable"));
		assert!(is_valid_namespace_mode(IPC_FIELD, "none"));
		for field in [PID_FIELD, UTS_FIELD, USERNS_FIELD, CGROUP_FIELD] {
			assert!(!is_valid_namespace_mode(field, "shareable"), "{field}");
			assert!(!is_valid_namespace_mode(field, "none"), "{field}");
		}
	}

	/// Measured against podman 5.7.0. `keep-id` is the standard rootless
	/// answer to a file-ownership mismatch, and podup rejected it outright
	/// (#1463). The option-carrying forms parse too.
	#[test]
	fn userns_takes_its_own_vocabulary() {
		for mode in [
			"keep-id",
			"auto",
			"nomap",
			"keep-id:uid=1000,gid=1000",
			"auto:size=65536",
		] {
			assert!(is_valid_namespace_mode(USERNS_FIELD, mode), "{mode}");
		}
		// And nowhere else.
		for field in [PID_FIELD, IPC_FIELD, UTS_FIELD, CGROUP_FIELD] {
			assert!(!is_valid_namespace_mode(field, "keep-id"), "{field}");
			assert!(!is_valid_namespace_mode(field, "auto"), "{field}");
			assert!(!is_valid_namespace_mode(field, "nomap"), "{field}");
		}
		// The option form still needs an option after the colon.
		assert!(!is_valid_namespace_mode(USERNS_FIELD, "keep-id:"));
		assert!(!is_valid_namespace_mode(USERNS_FIELD, "auto:"));
	}

	/// The error text has to name the modes the slot in hand accepts, not a
	/// union that would send the reader after a value their slot rejects.
	#[test]
	fn the_error_lists_the_modes_for_that_slot() {
		let userns = allowed_namespace_modes(USERNS_FIELD);
		assert!(userns.contains("keep-id"), "{userns}");
		assert!(!userns.contains("shareable"), "{userns}");

		let ipc = allowed_namespace_modes(IPC_FIELD);
		assert!(ipc.contains("shareable"), "{ipc}");
		assert!(!ipc.contains("keep-id"), "{ipc}");

		let pid = allowed_namespace_modes(PID_FIELD);
		assert!(!pid.contains("keep-id"), "{pid}");
		assert!(!pid.contains("shareable"), "{pid}");
	}

	#[test]
	fn first_invalid_namespace_returns_first_failure() {
		let slots: Vec<(&str, Option<&str>)> = vec![
			(PID_FIELD, Some("host")),
			(IPC_FIELD, Some("evil")),
			(UTS_FIELD, Some("private")),
		];
		let (field, value, allowed) = first_invalid_namespace(&slots).unwrap();
		assert_eq!(field, IPC_FIELD);
		assert_eq!(value, "evil");
		assert!(allowed.contains("`host`"));
		assert!(allowed.contains("`private`"));
		assert!(allowed.contains("`container:<id-or-path>`"));
	}

	#[test]
	fn first_invalid_namespace_passes_when_all_valid() {
		let slots: Vec<(&str, Option<&str>)> = vec![
			(PID_FIELD, Some("host")),
			(IPC_FIELD, Some("container:web")),
			(UTS_FIELD, None),
		];
		assert!(first_invalid_namespace(&slots).is_none());
	}

	#[test]
	fn device_access_accepts_rwm_subsets() {
		assert!(is_valid_device_access("r"));
		assert!(is_valid_device_access("w"));
		assert!(is_valid_device_access("m"));
		assert!(is_valid_device_access("rw"));
		assert!(is_valid_device_access("rwm"));
		assert!(is_valid_device_access("wm"));
	}

	#[test]
	fn device_access_rejects_invalid_chars() {
		assert!(!is_valid_device_access("x"));
		assert!(!is_valid_device_access("rwx"));
		assert!(!is_valid_device_access("r w"));
		assert!(!is_valid_device_access("rw "));
	}

	#[test]
	fn first_invalid_device_access_returns_index_and_field() {
		let rules = ["rwm", "rwmx", "rw"];
		let (field, value) = first_invalid_device_access(rules.iter().copied()).unwrap();
		assert_eq!(field, "device_cgroup_rule[1].access");
		assert_eq!(value, "rwmx");
	}

	#[test]
	fn first_invalid_device_access_skips_empty() {
		let rules = ["rwm", "", "rwx"];
		let (field, _) = first_invalid_device_access(rules.iter().copied()).unwrap();
		assert_eq!(field, "device_cgroup_rule[2].access");
	}

	#[test]
	fn first_invalid_device_access_passes_when_all_valid() {
		let rules = ["rwm", "r", "wm"];
		assert!(first_invalid_device_access(rules.iter().copied()).is_none());
	}

	#[test]
	fn kv_key_accepts_alnum_dot_dash_underscore() {
		assert!(is_valid_kv_key("FOO"));
		assert!(is_valid_kv_key("foo_bar"));
		assert!(is_valid_kv_key("FOO.BAR"));
		assert!(is_valid_kv_key("FOO-BAR"));
		assert!(is_valid_kv_key("a1b2c3"));
	}

	#[test]
	fn kv_key_rejects_invalid() {
		assert!(!is_valid_kv_key(""));
		assert!(!is_valid_kv_key("FOO BAR"));
		assert!(!is_valid_kv_key("FOO\nBAR"));
		assert!(!is_valid_kv_key("FOO=BAR"));
		assert!(!is_valid_kv_key("FOO;ls"));
	}

	#[test]
	fn first_invalid_kv_key_returns_first_failure() {
		let keys = ["GOOD", "MALFORMED\nKEY", "ALSO_GOOD"];
		let (field, key, msg) = first_invalid_kv_key("build.args", keys.iter().copied()).unwrap();
		assert_eq!(field, "build.args");
		assert_eq!(key, "MALFORMED\nKEY");
		assert!(msg.contains("not a valid identifier"));
	}

	#[test]
	fn first_invalid_kv_key_passes_when_all_valid() {
		let keys = ["GOOD", "ALSO_GOOD"];
		assert!(first_invalid_kv_key("build.args", keys.iter().copied()).is_none());
	}

	#[test]
	fn pre_validate_build_rejects_bad_key() {
		let mut args = HashMap::new();
		args.insert("MALFORMED\nKEY".to_string(), "value".to_string());
		let labels = HashMap::new();
		let err = pre_validate_build(&args, &labels).unwrap_err();
		let msg = err.to_string();
		assert!(msg.contains("build.args"));
		assert!(msg.contains("MALFORMED\\nKEY"));
	}

	#[test]
	fn pre_validate_build_rejects_bad_label() {
		let args = HashMap::new();
		let mut labels = HashMap::new();
		labels.insert("bad key".to_string(), "value".to_string());
		let err = pre_validate_build(&args, &labels).unwrap_err();
		let msg = err.to_string();
		assert!(msg.contains("build.labels"));
		assert!(msg.contains("bad key"));
	}

	#[test]
	fn pre_validate_build_passes_when_clean() {
		let mut args = HashMap::new();
		args.insert("GOOD".to_string(), "value".to_string());
		let mut labels = HashMap::new();
		labels.insert("GOOD_LABEL".to_string(), "value".to_string());
		assert!(pre_validate_build(&args, &labels).is_ok());
	}

	#[test]
	fn render_value_truncates_long_inputs() {
		let long = "x".repeat(500);
		let r = render_value(&long);
		assert!(r.len() <= 260);
		assert!(r.ends_with('…'));
	}

	#[test]
	fn render_value_escapes_control_chars() {
		assert_eq!(render_value("a\nb"), "a\\nb");
		assert_eq!(render_value("a\tb"), "a\\tb");
		assert_eq!(render_value("a\rb"), "a\\rb");
		assert!(render_value("a\x1bb").contains("\\u{1b}"));
	}

	#[test]
	fn render_value_keeps_normal_strings_intact() {
		assert_eq!(render_value("hello"), "hello");
		assert_eq!(render_value("CAP_NET_ADMIN"), "CAP_NET_ADMIN");
		assert_eq!(render_value("db:10.0.0.2"), "db:10.0.0.2");
	}

	#[test]
	fn build_field_error_uses_empty_service() {
		let e = build_field_error("build.args", "MALFORMED\nKEY", "podman rejected the key");
		match e {
			PodmanError::Field {
				service,
				field,
				value,
				message,
			} => {
				assert_eq!(service, "");
				assert_eq!(field, "build.args");
				assert_eq!(value, "MALFORMED\\nKEY");
				assert_eq!(message, "podman rejected the key");
			}
			_ => panic!("expected Field variant"),
		}
	}

	#[test]
	fn spec_field_error_carries_service() {
		let e = spec_field_error("web", "pid", "evil", "namespace not recognised");
		match e {
			PodmanError::Field {
				service,
				field,
				value,
				message,
			} => {
				assert_eq!(service, "web");
				assert_eq!(field, "pid");
				assert_eq!(value, "evil");
				assert_eq!(message, "namespace not recognised");
			}
			_ => panic!("expected Field variant"),
		}
	}
}
