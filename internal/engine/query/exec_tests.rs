use super::{
	expand_exec_env, is_exec_teardown_noise, map_exec_start_err, map_not_running, ExecOptions,
	EXEC_START_TIMEOUT,
};

#[test]
fn expand_exec_env_passes_through_key_value() {
	let out = expand_exec_env(&["FOO=bar".to_string(), "BAZ=qux".to_string()]);
	assert_eq!(out, vec!["FOO=bar".to_string(), "BAZ=qux".to_string()]);
}

#[test]
fn expand_exec_env_resolves_bare_key_from_host() {
	// A bare `KEY` takes its value from podup's own environment; an unset bare
	// key is dropped (libpod rejects a value-less env entry).
	std::env::set_var("PODUP_TEST_EXEC_ENV", "from-host");
	let out = expand_exec_env(&[
		"PODUP_TEST_EXEC_ENV".to_string(),
		"PODUP_TEST_EXEC_UNSET_ENV".to_string(),
	]);
	std::env::remove_var("PODUP_TEST_EXEC_ENV");
	assert_eq!(out, vec!["PODUP_TEST_EXEC_ENV=from-host".to_string()]);
}

#[test]
fn teardown_noise_matches_only_connection_reset_frame() {
	assert!(is_exec_teardown_noise(
		"read unixpacket @->/run/...: read: connection reset by peer"
	));
	// Ordinary program output is never suppressed.
	assert!(!is_exec_teardown_noise("connection reset by peer"));
	assert!(!is_exec_teardown_noise("hello world"));
}

#[test]
fn map_not_running_maps_404_and_stopped() {
	use crate::error::ComposeError;
	use crate::libpod::PodmanError;
	let e404 = PodmanError::Api {
		status: 404,
		message: "no such container: web".into(),
	};
	assert!(matches!(
		map_not_running(e404, "web"),
		ComposeError::NotRunning(s) if s == "web"
	));
	let e500 = PodmanError::Api {
		status: 500,
		message: "can only create exec sessions on running containers".into(),
	};
	assert!(matches!(
		map_not_running(e500, "web"),
		ComposeError::NotRunning(_)
	));
	// An unrelated error passes through unchanged.
	let other = PodmanError::Api {
		status: 500,
		message: "disk full".into(),
	};
	assert!(matches!(
		map_not_running(other, "web"),
		ComposeError::Podman(_)
	));
}

#[test]
fn exec_start_timeout_with_user_names_the_user() {
	use crate::libpod::PodmanError;
	// A client-side head timeout (the wedged-launch symptom) becomes a clear,
	// fast ExecFailed naming the likely culprit — never the raw socket-timeout.
	let timeout = PodmanError::Api {
		status: 0,
		message: format!(
			"timed out after {}s waiting for the Podman socket to respond",
			EXEC_START_TIMEOUT.as_secs()
		),
	};
	let opts = ExecOptions {
		user: Some("doesnotexist".into()),
		..Default::default()
	};
	let mapped = map_exec_start_err(timeout, &opts);
	match mapped {
		crate::error::ComposeError::ExecFailed(msg) => {
			assert!(msg.contains("doesnotexist"), "got {msg}");
			assert!(msg.contains("did not start"), "got {msg}");
			assert!(
				!msg.to_ascii_lowercase().contains("podman socket"),
				"must not leak the socket-timeout wording: {msg}"
			);
		}
		other => panic!("expected ExecFailed, got {other:?}"),
	}
}

#[test]
fn exec_start_timeout_without_user_names_the_workdir() {
	use crate::libpod::PodmanError;
	let timeout = PodmanError::Api {
		status: 0,
		message: "timed out after 20s waiting for the Podman socket to respond".into(),
	};
	let opts = ExecOptions {
		workdir: Some("/no/such/dir".into()),
		..Default::default()
	};
	match map_exec_start_err(timeout, &opts) {
		crate::error::ComposeError::ExecFailed(msg) => {
			assert!(msg.contains("/no/such/dir"), "got {msg}");
		}
		other => panic!("expected ExecFailed, got {other:?}"),
	}
}

#[test]
fn exec_start_real_api_error_passes_through() {
	use crate::error::ComposeError;
	use crate::libpod::PodmanError;
	// The prompt HTTP error an engine returns for a bad user is a genuine
	// diagnostic and must reach the user verbatim, not be rewritten.
	let api = PodmanError::Api {
		status: 500,
		message: "unable to find user doesnotexist: no matching entries in passwd file".into(),
	};
	let opts = ExecOptions {
		user: Some("doesnotexist".into()),
		..Default::default()
	};
	match map_exec_start_err(api, &opts) {
		ComposeError::Podman(e) => {
			assert!(e.to_string().contains("no matching entries in passwd file"));
		}
		other => panic!("expected Podman passthrough, got {other:?}"),
	}
}
