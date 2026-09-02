use super::*;
use crate::compose::types::Service;

#[test]
fn explicit_profiles_ignores_env() {
	let set = active_profiles_set(&["prod".to_string()]);
	assert!(set.contains("prod"));
	assert_eq!(set.len(), 1);
}

#[test]
fn empty_slice_with_no_env_returns_empty() {
	// Scope COMPOSE_PROFILES to "unset" race-free: `temp-env` serializes
	// the mutation and restores the prior value, avoiding the data race
	// that a bare `std::env::remove_var` carries under the parallel test
	// runner.
	temp_env::with_var_unset("COMPOSE_PROFILES", || {
		let set = active_profiles_set(&[]);
		assert!(set.is_empty());
	});
}

#[test]
fn empty_slice_falls_back_to_env_var() {
	// With no explicit profiles, COMPOSE_PROFILES is parsed: comma-separated,
	// each entry trimmed, empty entries dropped.
	temp_env::with_var("COMPOSE_PROFILES", Some(" debug , , prod "), || {
		let set = active_profiles_set(&[]);
		assert_eq!(set.len(), 2);
		assert!(set.contains("debug"));
		assert!(set.contains("prod"));
	});
}

#[test]
fn service_with_no_profiles_always_runs() {
	let svc = Service::default();
	let active: HashSet<String> = HashSet::new();
	assert!(service_in_profiles(&svc, &active));
}

#[test]
fn service_profile_matches_active() {
	let svc = Service {
		profiles: vec!["debug".to_string()],
		..Default::default()
	};
	let active: HashSet<String> = ["debug".to_string()].into();
	assert!(service_in_profiles(&svc, &active));
}

#[test]
fn service_profile_does_not_match() {
	let svc = Service {
		profiles: vec!["debug".to_string()],
		..Default::default()
	};
	let active: HashSet<String> = ["prod".to_string()].into();
	assert!(!service_in_profiles(&svc, &active));
}

#[test]
fn service_any_profile_match_sufficient() {
	let svc = Service {
		profiles: vec!["debug".to_string(), "prod".to_string()],
		..Default::default()
	};
	let active: HashSet<String> = ["prod".to_string()].into();
	assert!(service_in_profiles(&svc, &active));
}

#[test]
fn retain_active_profiles_keeps_unprofiled_and_active() {
	let yaml = "services:\n  \
		web:\n    image: x\n  \
		debugger:\n    image: x\n    profiles: [debug]\n  \
		db:\n    image: x\n    profiles: [prod]\n";
	// With `debug` active: the unprofiled `web` and the `debug` service stay,
	// the `prod`-only `db` is dropped — exactly what `up --profile debug` runs.
	let mut file = crate::parse_str(yaml).unwrap();
	retain_active_profiles(&mut file, &["debug".to_string()]);
	assert!(file.services.contains_key("web"));
	assert!(file.services.contains_key("debugger"));
	assert!(!file.services.contains_key("db"));

	// With no active profiles, every profiled service is dropped.
	let mut file = crate::parse_str(yaml).unwrap();
	temp_env::with_var_unset("COMPOSE_PROFILES", || {
		retain_active_profiles(&mut file, &[]);
	});
	assert!(file.services.contains_key("web"));
	assert_eq!(file.services.len(), 1);
}

#[test]
fn wildcard_enables_all_profiles() {
	// `--profile '*'` enables every profiled service, matching docker compose.
	let svc = Service {
		profiles: vec!["debug".to_string()],
		..Default::default()
	};
	let active: HashSet<String> = ["*".to_string()].into();
	assert!(service_in_profiles(&svc, &active));

	let yaml = "services:\n  \
		web:\n    image: x\n  \
		debugger:\n    image: x\n    profiles: [debug]\n  \
		db:\n    image: x\n    profiles: [prod]\n";
	let mut file = crate::parse_str(yaml).unwrap();
	retain_active_profiles(&mut file, &["*".to_string()]);
	assert_eq!(file.services.len(), 3);
}

#[test]
fn implicit_activation_keeps_profiled_dependency() {
	// `app` (active) depends on `db` (profiles: [storage]). With no profile
	// active, `db` is implicitly enabled so `app` keeps a satisfiable dep —
	// no dangling reference, matching docker compose.
	let yaml = "services:\n  \
		app:\n    image: x\n    depends_on: [db]\n  \
		db:\n    image: x\n    profiles: [storage]\n";
	let mut file = crate::parse_str(yaml).unwrap();
	temp_env::with_var_unset("COMPOSE_PROFILES", || {
		retain_active_profiles(&mut file, &[]);
	});
	assert!(file.services.contains_key("app"));
	assert!(
		file.services.contains_key("db"),
		"profiled depends_on target is implicitly activated"
	);
}

#[test]
fn implicit_activation_is_transitive() {
	// app -> db -> storage, where both db and storage are profiled. Enabling
	// app must pull in the whole transitive dependency chain.
	let yaml = "services:\n  \
		app:\n    image: x\n    depends_on: [db]\n  \
		db:\n    image: x\n    profiles: [p]\n    depends_on: [storage]\n  \
		storage:\n    image: x\n    profiles: [q]\n";
	let mut file = crate::parse_str(yaml).unwrap();
	temp_env::with_var_unset("COMPOSE_PROFILES", || {
		retain_active_profiles(&mut file, &[]);
	});
	assert_eq!(file.services.len(), 3);
}

#[test]
fn unrelated_profiled_service_still_dropped() {
	// Implicit activation only reaches dependencies — an unrelated profiled
	// service is still removed.
	let yaml = "services:\n  \
		app:\n    image: x\n    depends_on: [db]\n  \
		db:\n    image: x\n    profiles: [storage]\n  \
		extra:\n    image: x\n    profiles: [other]\n";
	let mut file = crate::parse_str(yaml).unwrap();
	temp_env::with_var_unset("COMPOSE_PROFILES", || {
		retain_active_profiles(&mut file, &[]);
	});
	assert!(file.services.contains_key("db"));
	assert!(!file.services.contains_key("extra"));
}

#[test]
fn enabled_set_activates_profiled_dependency_for_up() {
	// The `up`/`create` lifecycle path consults this set directly. `app`
	// (unprofiled, started) depends on `db` (profiles: [storage]). With no
	// profile active, `db` must be in the enabled set so `up` actually
	// creates it — otherwise `app` runs with an unsatisfied dependency.
	let yaml = "services:\n  \
		app:\n    image: x\n    depends_on: [db]\n  \
		db:\n    image: x\n    profiles: [storage]\n";
	let file = crate::parse_str(yaml).unwrap();
	let active: HashSet<String> = HashSet::new();
	let enabled = enabled_profile_services(&file, &active, &[]);
	assert!(enabled.contains("app"));
	assert!(
		enabled.contains("db"),
		"profiled depends_on target is in the started set"
	);
}

#[test]
fn enabled_set_excludes_unrelated_profiled_service() {
	// Only dependencies are pulled in — an unrelated profiled service stays
	// out of the started set, so `up` does not over-activate it.
	let yaml = "services:\n  \
		app:\n    image: x\n    depends_on: [db]\n  \
		db:\n    image: x\n    profiles: [storage]\n  \
		extra:\n    image: x\n    profiles: [other]\n";
	let file = crate::parse_str(yaml).unwrap();
	let active: HashSet<String> = HashSet::new();
	let enabled = enabled_profile_services(&file, &active, &[]);
	assert!(enabled.contains("app"));
	assert!(enabled.contains("db"));
	assert!(!enabled.contains("extra"));
}

#[test]
fn named_target_keeps_inactive_profile_service() {
	// Naming a profiled service on the command line activates its profile, so
	// per-service subcommands can still address it.
	let yaml = "services:\n  \
		web:\n    image: x\n  \
		debugger:\n    image: x\n    profiles: [debug]\n";
	let mut file = crate::parse_str(yaml).unwrap();
	temp_env::with_var_unset("COMPOSE_PROFILES", || {
		retain_active_profiles_with_targets(&mut file, &[], &["debugger".to_string()]);
	});
	assert!(file.services.contains_key("web"));
	assert!(file.services.contains_key("debugger"));
}
