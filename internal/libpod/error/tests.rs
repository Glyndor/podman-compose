use crate::libpod::error::{conflict_hint, PodmanError};

#[test]
fn conflict_hint_recognises_common_state_errors() {
	let rm = "cannot remove container abc as it is running - running or paused containers \
			cannot be removed without force: container state improper";
	assert!(conflict_hint(rm).unwrap().contains("-f"));
	assert!(conflict_hint("container abc is already paused")
		.unwrap()
		.contains("already paused"));
	assert!(conflict_hint("container abc is not paused")
		.unwrap()
		.contains("not paused"));
	assert!(
		conflict_hint("cannot kill container abc: container abc is not running")
			.unwrap()
			.contains("not running")
	);
	// libpod's kill-of-stopped 409 message ("can only kill running
	// containers …") gets the same friendly "not running" hint.
	assert!(conflict_hint(
		"can only kill running containers. abc is in state exited: container state improper"
	)
	.unwrap()
	.contains("not running"));
}

#[test]
fn is_kill_of_stopped_matches_only_the_kill_409() {
	let stopped = PodmanError::Api {
		status: 409,
		message: "can only kill running containers. abc is in state exited: \
				container state improper"
			.into(),
	};
	assert!(stopped.is_kill_of_stopped());
	// A different 409 (e.g. already-paused) must not be swallowed by kill.
	let paused = PodmanError::Api {
		status: 409,
		message: "container abc is already paused".into(),
	};
	assert!(!paused.is_kill_of_stopped());
	// Wrong status, even with a matching message, is not a kill-of-stopped.
	let other = PodmanError::Api {
		status: 500,
		message: "can only kill running containers".into(),
	};
	assert!(!other.is_kill_of_stopped());
	assert!(
		!PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err()).is_kill_of_stopped()
	);
}

#[test]
fn conflict_hint_paused_rm_is_not_labelled_running() {
	// Podman's removal refusal for a *paused* container shares the "without
	// force" wording; the hint must say paused, not running.
	let paused = "cannot remove container abc as it is paused - running or paused containers \
			cannot be removed without force: container state improper";
	let hint = conflict_hint(paused).unwrap();
	assert!(hint.contains("paused"), "got {hint:?}");
	assert!(!hint.contains("running"), "must not say running: {hint:?}");
	assert!(hint.contains("-f"));
}

#[test]
fn conflict_hint_covers_kill_exec_and_start() {
	// kill a stopped container.
	assert!(
		conflict_hint("can only kill running containers. abc is in state exited")
			.unwrap()
			.contains("not running")
	);
	// exec into a stopped container.
	assert!(conflict_hint(
		"can only create exec sessions on running containers: container state improper"
	)
	.unwrap()
	.contains("not running"));
	// start a container that is not startable.
	assert!(conflict_hint(
		"unable to start container abc: container must be in Created or Stopped state to be \
			 started"
	)
	.unwrap()
	.contains("cannot be started"));
}

#[test]
fn conflict_hint_none_for_unrecognised() {
	assert!(conflict_hint("some unrelated error").is_none());
	assert!(conflict_hint("no such container: abc").is_none());
}

#[test]
fn api_error_display_leads_with_hint_and_keeps_message() {
	let e = PodmanError::Api {
		status: 409,
		message: "container abc is already paused".into(),
	};
	let s = e.to_string();
	assert!(s.starts_with("the container is already paused"));
	assert!(s.contains("podman: container abc is already paused"));
}

#[test]
fn api_error_display_raw_when_no_hint() {
	let e = PodmanError::Api {
		status: 500,
		message: "boom".into(),
	};
	assert_eq!(e.to_string(), "podman API error (HTTP 500): boom");
}

#[test]
fn is_status_matches_code() {
	let e = PodmanError::Api {
		status: 404,
		message: "not found".into(),
	};
	assert!(e.is_status(404));
	assert!(!e.is_status(200));
	assert!(!e.is_status(500));
}

#[test]
fn is_timeout_matches_synthetic_timeout_error() {
	// Client-side timeouts carry status 0 and a "timed out" message; lifecycle
	// stop escalation keys off this.
	assert!(PodmanError::Api {
		status: 0,
		message: "timed out after 40s waiting for the Podman socket to respond".into(),
	}
	.is_timeout());
	// A real HTTP error (non-zero status) is not a timeout.
	assert!(!PodmanError::Api {
		status: 500,
		message: "boom".into(),
	}
	.is_timeout());
	// A status-0 error without the timeout marker is not a timeout.
	assert!(!PodmanError::Api {
		status: 0,
		message: "invalid API path".into(),
	}
	.is_timeout());
}

#[test]
fn is_status_false_for_non_api() {
	let e = PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err());
	assert!(!e.is_status(404));
}

#[test]
fn already_exists_accepts_409_and_500_with_message() {
	// 409 conflict: always an already-exists.
	assert!(PodmanError::Api {
		status: 409,
		message: "network already used".into(),
	}
	.is_already_exists());
	// 500 carrying the libpod "already exists" cause (Podman's volume-create
	// path) must also count as already-exists for idempotent create.
	assert!(PodmanError::Api {
		status: 500,
		message: "volume with name p_v already exists: volume already exists".into(),
	}
	.is_already_exists());
}

#[test]
fn image_in_use_accepts_409_and_500_with_message() {
	// A 409 on image delete is always an in-use conflict.
	assert!(PodmanError::Api {
		status: 409,
		message: "image is in use by 1 container".into(),
	}
	.is_image_in_use());
	// Some Podman versions report it as a 500 naming the cause.
	assert!(PodmanError::Api {
		status: 500,
		message: "image used by a container: image in use".into(),
	}
	.is_image_in_use());
	// An unrelated 500 still propagates.
	assert!(!PodmanError::Api {
		status: 500,
		message: "internal error".into(),
	}
	.is_image_in_use());
	assert!(!PodmanError::Api {
		status: 404,
		message: "no such image".into(),
	}
	.is_image_in_use());
}

#[test]
fn state_conflict_recognises_pause_unpause_mismatches() {
	for msg in [
		"container abc is already paused: container state improper",
		"container abc is not paused: container state improper",
		"cannot pause container abc: container abc is not running",
		"unpausing container: container state improper",
	] {
		assert!(
			PodmanError::Api {
				status: 500,
				message: msg.into(),
			}
			.is_state_conflict(),
			"should treat {msg:?} as a state conflict"
		);
	}
	// A genuine failure is not a state conflict.
	assert!(!PodmanError::Api {
		status: 500,
		message: "internal error".into(),
	}
	.is_state_conflict());
	assert!(!PodmanError::Api {
		status: 404,
		message: "no such container".into(),
	}
	.is_state_conflict());
}

#[test]
fn already_exists_false_for_other_errors() {
	// A 500 that is not an already-exists must still propagate.
	assert!(!PodmanError::Api {
		status: 500,
		message: "internal error".into(),
	}
	.is_already_exists());
	assert!(!PodmanError::Api {
		status: 404,
		message: "no such volume".into(),
	}
	.is_already_exists());
	assert!(!PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err()).is_already_exists());
}

#[test]
fn display_api_error() {
	let e = PodmanError::Api {
		status: 500,
		message: "internal error".into(),
	};
	assert_eq!(e.to_string(), "podman API error (HTTP 500): internal error");
}

#[test]
fn display_json_error() {
	let e = PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err());
	assert!(e.to_string().contains("json error"));
}

#[test]
fn display_connect_error() {
	let e = PodmanError::Connect(std::io::Error::new(
		std::io::ErrorKind::NotFound,
		"no socket",
	));
	assert!(e.to_string().contains("podman socket connection error"));
}

#[test]
fn display_incompatible_api_version_reports_version() {
	let e = PodmanError::IncompatibleApiVersion {
		reported: "4.9.3".into(),
	};
	let msg = e.to_string();
	assert!(msg.contains("Podman >= 5.0"));
	assert!(msg.contains("4.9.3"));
}

#[test]
fn display_incompatible_api_version_handles_missing_header() {
	// An empty reported version (no `Libpod-API-Version` header) renders a
	// readable placeholder rather than a blank.
	let e = PodmanError::IncompatibleApiVersion {
		reported: String::new(),
	};
	let msg = e.to_string();
	assert!(msg.contains("an unknown version"));
}

#[test]
fn source_present_for_wrapped_errors_absent_for_owned() {
	use std::error::Error;
	// Wrapped lower-level errors expose their source...
	let connect = PodmanError::Connect(std::io::Error::new(
		std::io::ErrorKind::NotFound,
		"no socket",
	));
	assert!(connect.source().is_some());
	let json = PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err());
	assert!(json.source().is_some());
	// ...while the owned variants have no underlying source.
	assert!(PodmanError::Api {
		status: 500,
		message: "x".into(),
	}
	.source()
	.is_none());
	assert!(PodmanError::IncompatibleApiVersion {
		reported: "4.0.0".into(),
	}
	.source()
	.is_none());
}

#[test]
fn from_io_error_becomes_connect() {
	// The `?`-operator conversion an io error takes when bubbling out of the
	// client maps onto the Connect variant.
	let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
	let e: PodmanError = io.into();
	assert!(matches!(e, PodmanError::Connect(_)));
	assert!(e.to_string().contains("podman socket connection error"));
}

#[test]
fn from_json_error_becomes_json() {
	let json_err = serde_json::from_str::<u8>("not-json").unwrap_err();
	let e: PodmanError = json_err.into();
	assert!(matches!(e, PodmanError::Json(_)));
	assert!(e.to_string().contains("json error"));
}

#[test]
fn field_error_names_service_and_field() {
	// A `Field` rendered for a container-create failure leads with the
	// service and field, then the value, so the operator sees the
	// offending entry without parsing the libpod body (#1357).
	let e = PodmanError::Field {
		service: "web".into(),
		field: "pid".into(),
		value: "evil".into(),
		message: "namespace not recognised".into(),
	};
	let msg = e.to_string();
	assert!(
		msg.starts_with("service.web: pid: namespace not recognised"),
		"got {msg:?}"
	);
	assert!(msg.contains("evil"));
}

#[test]
fn field_error_for_build_query_skips_service_prefix() {
	// Build queries have no service context, so the `service.X:` prefix
	// is omitted and the field name leads directly.
	let e = PodmanError::Field {
		service: String::new(),
		field: "build.args".into(),
		value: "MALFORMED\\nKEY".into(),
		message: "key is not a valid identifier".into(),
	};
	let msg = e.to_string();
	assert!(
		msg.starts_with("build.args: key is not a valid identifier"),
		"got {msg:?}"
	);
	assert!(!msg.contains("service."));
	assert!(msg.contains("MALFORMED\\nKEY"));
}

#[test]
fn field_error_has_no_source() {
	// Like the other owned variants, `Field` has no underlying error to
	// chain — the explanation is in the variant's own fields.
	use std::error::Error;
	let e = PodmanError::Field {
		service: "web".into(),
		field: "pid".into(),
		value: "evil".into(),
		message: "namespace not recognised".into(),
	};
	assert!(e.source().is_none());
}

#[test]
fn is_status_does_not_match_field() {
	// The `is_status` predicate keys on the `Api` variant, so a `Field`
	// rejection is never mistaken for a generic API error.
	let e = PodmanError::Field {
		service: "web".into(),
		field: "pid".into(),
		value: "evil".into(),
		message: "x".into(),
	};
	assert!(!e.is_status(400));
	assert!(!e.is_status(500));
}
