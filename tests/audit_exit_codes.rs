//! End-to-end checks for the `podup audit` subcommand's exit codes and JSON
//! shape. Drives the compiled `podup` binary against a tiny compose file on
//! disk so the parsing, profile honouring, and emit path are exercised the
//! same way an operator would; the unit suite already covers the checks
//! themselves in isolation.
//!
//! Names state the contract: `--strict` flips the exit code from 0
//! to 1 when any finding is present, and `--format json` is the
//! machine-readable surface CI consumes.

use std::fs;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// Render `body` to a unique tempfile under the system tempdir and return
/// its path. The audit command is invoked with `-f <path>` so the compose
/// file lives exactly where the operator would point `-f`. Every call gets
/// a freshly-numbered subdirectory; the counter is atomic so two threads
/// running the same test in parallel never share a directory.
fn write_compose(body: &str) -> std::path::PathBuf {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let n = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir().join(format!("podup-audit-{}-{n}", std::process::id()));
	let _ = fs::remove_dir_all(&dir);
	fs::create_dir_all(&dir).unwrap();
	let path = dir.join("compose.yaml");
	fs::write(&path, body).unwrap();
	path
}

fn run(args: &[&str]) -> Output {
	let mut cmd = Command::new(bin());
	cmd.args(args);
	// The integration tests share the developer's shell environment, so any
	// pre-set `PODUP_*` / `COMPOSE_*` would leak into the spawned binary and
	// break parsing or change the resolved project. Strip the env vars podup
	// treats as global configuration; `PATH` and the locale stay so the
	// process can locate shared libraries and render messages.
	for key in [
		"PODUP_LIBPOD_POOL",
		// `PODUP_LIBCOD_POOL` is the legacy typo'd spelling; the runtime
		// still reads it as a fallback so a developer's exported value
		// would otherwise leak into the spawned binary and silently
		// override its default pool size.
		"PODUP_LIBCOD_POOL",
		"PODMAN_SOCKET",
		"DOCKER_HOST",
		"COMPOSE_PROJECT_NAME",
		"COMPOSE_PROFILES",
		"COMPOSE_FILE",
		"NO_COLOR",
	] {
		cmd.env_remove(key);
	}
	cmd.output().expect("run podup audit")
}

// ---------------------------------------------------------------------------
// exit codes
// ---------------------------------------------------------------------------

#[test]
fn audit_exits_zero_with_findings_by_default() {
	// A trivially unhardened service: every check that fires on a bare
	// `image: nginx` will surface. Without `--strict` the exit code must
	// stay 0, the same way `ps` exits 0 on a stopped project, so a CI
	// pipeline that has not opted in sees the report without breaking.
	let path = write_compose("services:\n  web:\n    image: nginx\n    privileged: true\n");
	let p = path.to_str().unwrap();
	let out = run(&["-f", p, "audit"]);
	assert!(
		out.status.success(),
		"`audit` must default to exit 0; got {:?}\nstderr: {}\nstdout: {}",
		out.status.code(),
		String::from_utf8_lossy(&out.stderr),
		String::from_utf8_lossy(&out.stdout),
	);
	// The table is the whole point of the default run; a sweep that emptied
	// the renderer left this test green, so the output is read too.
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("SERVICE") && stdout.contains("FINDINGS"),
		"no table header:\n{stdout}"
	);
	assert!(
		stdout.contains("writable_root"),
		"no finding named in the table:\n{stdout}"
	);
	assert!(
		stdout.contains(": writable_root: "),
		"no reason line under the table:\n{stdout}"
	);
}

#[test]
fn audit_strict_exits_one_with_findings() {
	// Same compose file, `--strict` enabled: the same findings should now
	// flip the exit to 1. CI scripts pipe `--strict` into a job's success
	// gate, so this is the property they care about.
	let path = write_compose("services:\n  web:\n    image: nginx\n    privileged: true\n");
	let p = path.to_str().unwrap();
	let out = run(&["-f", p, "audit", "--strict"]);
	assert_eq!(
		out.status.code(),
		Some(1),
		"`--strict` with findings must exit 1; got {:?}\nstderr: {}\nstdout: {}",
		out.status.code(),
		String::from_utf8_lossy(&out.stderr),
		String::from_utf8_lossy(&out.stdout),
	);
}

#[test]
fn audit_strict_exits_zero_when_clean() {
	// A fully hardened service must exit 0 even with `--strict`, otherwise
	// the CI-gate promise is "fail forever" rather than "fail when there
	// is something to fix". The unit suite already pins per-check
	// pass/warn behaviour; this is the CLI-level sum of that.
	let path = write_compose(
		r#"services:
  web:
    image: nginx:1.27@sha256:0e7bb5afc7e5e22ee46c4f2cd4a8b3fa63ad3f5d5e5e5e5e5e5e5e5e5e5e5e5e
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    pids_limit: 200
    mem_limit: 512m
    userns_mode: auto
    environment:
      - LOG_LEVEL=info
"#,
	);
	let p = path.to_str().unwrap();
	let out = run(&["-f", p, "audit", "--strict"]);
	assert!(
		out.status.success(),
		"`--strict` with no findings must exit 0; got {:?}\nstderr: {}\nstdout: {}",
		out.status.code(),
		String::from_utf8_lossy(&out.stderr),
		String::from_utf8_lossy(&out.stdout),
	);
}

// ---------------------------------------------------------------------------
// JSON shape: every finding listed, stable schema, no escapes.
// ---------------------------------------------------------------------------

#[test]
fn audit_json_lists_every_finding() {
	// Three services, each deliberately failing a different check. The
	// JSON output must carry every one of them: a regression where a check
	// silently no-ops in JSON (while still appearing in the table) would
	// pass the unit tests and break CI consumers in production.
	let path = write_compose(
		r#"services:
  pr:
    image: nginx
  caps:
    image: nginx:1.27
    cap_add: [SYS_ADMIN]
  secret:
    image: nginx:1.27
    environment:
      - DB_PASSWORD=hunter2
"#,
	);
	let p = path.to_str().unwrap();
	let out = run(&["-f", p, "audit", "--format", "json"]);
	assert!(out.status.success(), "audit must succeed");
	let stdout = String::from_utf8_lossy(&out.stdout);
	// Machine output never carries colour, even when
	// forced. A regression that leaks an escape into `--format json`
	// would corrupt every CI consumer parsing the output; verify the
	// unforced (no `--ansi always`) path first, then assert the data.
	assert!(
		!stdout.contains('\u{1b}'),
		"JSON output must not carry escapes: {stdout:?}"
	);
	let v: serde_json::Value = serde_json::from_str(&stdout).expect("JSON parses");
	let arr = v
		.get("findings")
		.and_then(|f| f.as_array())
		.expect("`findings` array");
	// Each service is expected to fire at least one finding; json-list
	// must reflect them all.
	assert!(arr.len() >= 3, "expected at least 3 findings, got {arr:?}");
	let services: Vec<&str> = arr
		.iter()
		.map(|f| f.get("service").and_then(|s| s.as_str()).unwrap_or("?"))
		.collect();
	for needed in ["pr", "caps", "secret"] {
		assert!(
			services.contains(&needed),
			"missing finding for `{needed}` in {arr:?}"
		);
	}
	// Per-object shape: every entry must carry all three keys, with
	// non-empty strings. A consumer reading `reason: ""` as "no finding"
	// would mis-classify an empty row, so we pin the absence.
	for entry in arr {
		for key in ["service", "check", "reason"] {
			let s = entry.get(key).and_then(|v| v.as_str()).unwrap_or("");
			assert!(
				!s.is_empty(),
				"every finding must carry non-empty `{key}`: {entry:?}"
			);
		}
	}
}
