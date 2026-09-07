use super::*;

#[test]
fn safe_unit_stem_neutralizes_traversal_and_control_chars() {
	assert_eq!(safe_unit_stem("web"), "web");
	assert_eq!(safe_unit_stem("db-data_1.x"), "db-data_1.x");
	assert_eq!(safe_unit_stem("../../etc/passwd"), "_.._.._etc_passwd");
	assert_eq!(safe_unit_stem("/abs"), "_abs");
	assert_eq!(safe_unit_stem(".hidden"), "_.hidden");
	assert_eq!(safe_unit_stem(""), "_");
	assert!(!safe_unit_stem("a\nb").contains('\n'));
}

#[test]
fn sanitize_value_strips_control_characters() {
	assert_eq!(sanitize_value("plain"), "plain");
	assert_eq!(sanitize_value("a\nb\tc\r"), "abc");
}

#[test]
fn render_command_exec_quotes_args_with_whitespace() {
	let cmd = Command::Exec(vec![
		"server".to_string(),
		"--port".to_string(),
		"9000".to_string(),
	]);
	// Plain arguments pass through unquoted.
	assert_eq!(render_command(&cmd), "server --port 9000");

	let cmd = Command::Exec(vec!["echo".to_string(), "hello world".to_string()]);
	assert_eq!(render_command(&cmd), "echo \"hello world\"");
}

#[test]
fn render_command_exec_multiline_arg_stays_one_line() {
	// A multi-line block-scalar argument (e.g. an `sh -c` script) must be
	// quoted with its newlines C-escaped so the rendered Exec= never spills
	// onto a second physical line or mashes adjacent tokens together.
	let cmd = Command::Exec(vec![
		"sh".to_string(),
		"-c".to_string(),
		"mkdir -p /www\necho hi > /www/index.html\nexec httpd".to_string(),
	]);
	let out = render_command(&cmd);
	assert!(
		!out.contains('\n'),
		"rendered Exec must be a single line: {out}"
	);
	assert_eq!(
		out,
		"sh -c \"mkdir -p /www\\necho hi > /www/index.html\\nexec httpd\""
	);
}

#[test]
fn render_command_exec_escapes_quotes_and_backslashes() {
	let cmd = Command::Exec(vec![
		"sh".to_string(),
		"-c".to_string(),
		"printf '%s' \"a\\b\"".to_string(),
	]);
	let out = render_command(&cmd);
	assert!(!out.contains('\n'));
	// The embedded double quotes and backslash are escaped inside the quoted
	// argument.
	assert!(out.starts_with("sh -c \""));
	assert!(out.contains("\\\""));
	assert!(out.contains("\\\\"));
}

#[test]
fn render_command_shell_neutralizes_newlines() {
	let cmd = Command::Shell("echo a\necho b".to_string());
	let out = render_command(&cmd);
	assert!(!out.contains('\n'));
	assert_eq!(out, "echo a\\necho b");

	// A plain single-line shell command is unchanged.
	assert_eq!(
		render_command(&Command::Shell("echo hi".to_string())),
		"echo hi"
	);
}

// --- render_publish_port ---

#[test]
fn render_publish_port_full_and_partial_forms() {
	let port = |host_ip: &str, host_port: Option<u16>, cp: u16, proto: &str| ParsedPort {
		container_port: cp,
		protocol: proto.to_string(),
		host_ip: host_ip.to_string(),
		host_port,
	};
	// ip + host port + container port, default tcp omits the protocol suffix.
	assert_eq!(
		render_publish_port(&port("127.0.0.1", Some(8080), 80, "tcp")),
		"127.0.0.1:8080:80"
	);
	// No ip, no host port (runtime-assigned) → bare container port.
	assert_eq!(render_publish_port(&port("", None, 80, "tcp")), "80");
	// A non-tcp protocol is appended; a 0 host port is treated as "let Podman pick".
	assert_eq!(render_publish_port(&port("", Some(0), 53, "udp")), "53/udp");
}

// --- render_volume ---

#[test]
fn render_volume_short_declared_uses_dot_volume_with_options() {
	let out = render_volume(
		&VolumeMount::Short("data:/app:ro".into()),
		"proj",
		&["data"],
	);
	// A declared named volume becomes `<project>-<name>.volume:<target>:<opts>`.
	assert_eq!(out, "proj-data.volume:/app:ro");
	// An undeclared source is passed through verbatim.
	assert_eq!(
		render_volume(&VolumeMount::Short("./host:/app".into()), "proj", &["data"]),
		"./host:/app"
	);
}

#[test]
fn render_volume_long_collects_options_and_handles_empty_source() {
	use crate::compose::types::VolumeOptions;
	let nocopy = VolumeMount::Long {
		volume_type: VolumeType::Volume,
		source: Some("vol".into()),
		target: "/data".into(),
		read_only: Some(true),
		bind: None,
		volume: Some(VolumeOptions {
			nocopy: Some(true),
			..Default::default()
		}),
		tmpfs: None,
		consistency: None,
	};
	// Declared → project-prefixed `.volume` suffix; ro + nocopy folded into
	// the options field.
	assert_eq!(
		render_volume(&nocopy, "proj", &["vol"]),
		"proj-vol.volume:/data:ro,nocopy"
	);

	// The hardening trio survives the Quadlet export (#1160): a compose
	// file that hardens a mount must not export a unit that does not.
	let hardened = VolumeMount::Long {
		volume_type: VolumeType::Volume,
		source: Some("vol".into()),
		target: "/data".into(),
		read_only: None,
		bind: None,
		volume: Some(VolumeOptions {
			noexec: Some(true),
			nosuid: Some(true),
			nodev: Some(true),
			..Default::default()
		}),
		tmpfs: None,
		consistency: None,
	};
	assert_eq!(
		render_volume(&hardened, "proj", &["vol"]),
		"proj-vol.volume:/data:noexec,nosuid,nodev"
	);

	// An empty source renders as just the target (anonymous mount).
	let anon = VolumeMount::Long {
		volume_type: VolumeType::Volume,
		source: None,
		target: "/scratch".into(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: None,
		consistency: None,
	};
	assert_eq!(render_volume(&anon, "proj", &[]), "/scratch");
}

// --- render_tmpfs_mount ---

#[test]
fn render_tmpfs_mount_with_and_without_options() {
	use crate::compose::types::TmpfsOptions;
	let with_opts = VolumeMount::Long {
		volume_type: VolumeType::Tmpfs,
		source: None,
		target: "/cache".into(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: Some(TmpfsOptions {
			size: Some(4096),
			mode: Some(0o700),
		}),
		consistency: None,
	};
	assert_eq!(
		render_tmpfs_mount(&with_opts).as_deref(),
		Some("/cache:size=4096,mode=700")
	);

	// A tmpfs mount with no size/mode renders just the target.
	let bare = VolumeMount::Long {
		volume_type: VolumeType::Tmpfs,
		source: None,
		target: "/run".into(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: None,
		consistency: None,
	};
	assert_eq!(render_tmpfs_mount(&bare).as_deref(), Some("/run"));

	// A non-tmpfs mount returns None so the caller emits a normal Volume=.
	assert!(render_tmpfs_mount(&VolumeMount::Short("a:/b".into())).is_none());
}

#[test]
fn render_tmpfs_mount_bare_decimal_mode_is_not_octal_re_encoded() {
	// Regression for #917: a long-form tmpfs with a bare `mode: 700` must
	// render `mode=700`, not `mode=1274` (700 octal-encoded a second time).
	let yaml = "type: tmpfs\ntarget: /run\ntmpfs:\n  mode: 700\n";
	let mount: VolumeMount = serde_yaml::from_str(yaml).expect("parse tmpfs mount");
	assert_eq!(render_tmpfs_mount(&mount).as_deref(), Some("/run:mode=700"));
}

// --- escape_unit_value (bug: incomplete systemd value escaping) ---

#[test]
fn escape_word_split_value_with_whitespace_is_quoted() {
	// An Environment value with whitespace must be quoted so systemd keeps it
	// as one value instead of splitting it into bogus extra entries.
	assert_eq!(
		escape_unit_value("Environment", "JAVA_OPTS=-Xmx512m -Xms256m"),
		"\"JAVA_OPTS=-Xmx512m -Xms256m\""
	);
	// A Label is word-split too.
	assert_eq!(
		escape_unit_value("Label", "note=hello world"),
		"\"note=hello world\""
	);
	// A scalar key that is not word-split keeps whitespace unquoted.
	assert_eq!(
		escape_unit_value("Description", "web (podup)"),
		"web (podup)"
	);
}

#[test]
fn escape_trailing_backslash_is_quoted_and_escaped() {
	// A value ending in a backslash would otherwise continue onto, and
	// swallow, the next directive line; it must be quoted and escaped.
	let out = escape_unit_value("Environment", "WINPATH=C:\\tmp\\");
	assert!(!out.ends_with('\\') || out.ends_with("\\\""));
	assert_eq!(out, "\"WINPATH=C:\\\\tmp\\\\\"");
}

#[test]
fn escape_percent_is_doubled_for_literal() {
	// systemd specifiers like %h must be passed through literally, not
	// expanded at unit-activation time, matching docker-compose semantics.
	assert_eq!(escape_unit_value("Environment", "HOME=%h"), "HOME=%%h");
	assert_eq!(escape_unit_value("Image", "img%U"), "img%%U");
}

#[test]
fn escape_arg_line_keys_are_left_intact() {
	// PodmanArgs/Exec/Entrypoint encode their own whitespace splitting and
	// must not be quoted or have `%` doubled.
	assert_eq!(
		escape_unit_value("PodmanArgs", "--security-opt apparmor=foo"),
		"--security-opt apparmor=foo"
	);
	assert_eq!(
		escape_unit_value("Exec", "sh -c \"echo %s\""),
		"sh -c \"echo %s\""
	);
}

// --- quote_podman_arg_value (smuggle guard for the seven sites) ---

#[test]
fn quote_podman_arg_value_always_quotes_and_doubles_percent() {
	// The seven interpolation sites call this on a raw compose value before
	// splicing it into a `--flag=value` template. The result must be one
	// systemd-token, regardless of whether the value has whitespace.
	assert_eq!(quote_podman_arg_value("512m"), "\"512m\"");
	// A value carrying whitespace must still be one quoted token; the
	// smuggled `--privileged` would otherwise become its own argv element.
	let quoted = quote_podman_arg_value("0 --privileged -v /:/hostfs2");
	assert_eq!(quoted, "\"0 --privileged -v /:/hostfs2\"");
}

#[test]
fn quote_podman_arg_value_strips_control_characters() {
	// A newline injection must be flattened; the sanitiser strips it so the
	// value stays one physical line inside the PodmanArgs= directive.
	assert_eq!(
		quote_podman_arg_value("ok\nExecStartPre=/bin/rm -rf /"),
		"\"okExecStartPre=/bin/rm -rf /\""
	);
}

#[test]
fn quote_podman_arg_value_doubles_percent_for_literal() {
	// systemd specifiers like %h must not be expanded; doubling to %%h is
	// what Environment= does (#1734). podman receives the literal `%h`.
	assert_eq!(quote_podman_arg_value("%h/mem"), "\"%%h/mem\"");
}

#[test]
fn quote_podman_arg_value_escapes_backslash_and_quote() {
	// A backslash inside the quoted group would fold the next physical
	// line, swallowing whatever directive follows; an unescaped `"` would
	// terminate the quoted group early.
	assert_eq!(quote_podman_arg_value("back\\slash"), "\"back\\\\slash\"");
	assert_eq!(quote_podman_arg_value("with\"quote"), "\"with\\\"quote\"");
}

#[test]
fn safe_unit_stem_strips_leading_dash() {
	// A name starting with `-`/`--` must not yield a file name beginning with
	// a dash (a globbing/flag-injection hazard for downstream tooling).
	assert_eq!(safe_unit_stem("--foo"), "_--foo");
	assert_eq!(safe_unit_stem("-x"), "_-x");
	assert!(!safe_unit_stem("--foo").starts_with('-'));
}

#[test]
fn unit_stem_is_project_prefixed() {
	assert_eq!(unit_stem("proj", "web"), "proj-web");
	// A leading dash in the project still cannot produce a dash-leading stem.
	assert!(!unit_stem("-p", "web").starts_with('-'));
}
