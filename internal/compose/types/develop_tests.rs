use super::*;

#[test]
fn watch_action_sync() {
	let a: WatchAction = serde_yaml::from_str("\"sync\"").unwrap();
	assert_eq!(a, WatchAction::Sync);
}

#[test]
fn watch_action_rebuild() {
	let a: WatchAction = serde_yaml::from_str("\"rebuild\"").unwrap();
	assert_eq!(a, WatchAction::Rebuild);
}

#[test]
fn watch_action_restart() {
	let a: WatchAction = serde_yaml::from_str("\"restart\"").unwrap();
	assert_eq!(a, WatchAction::Restart);
}

#[test]
fn watch_action_sync_and_restart() {
	let a: WatchAction = serde_yaml::from_str("\"sync+restart\"").unwrap();
	assert_eq!(a, WatchAction::SyncAndRestart);
}

#[test]
fn watch_action_sync_and_exec() {
	let a: WatchAction = serde_yaml::from_str("\"sync+exec\"").unwrap();
	assert_eq!(a, WatchAction::SyncAndExec);
}

#[test]
fn watch_action_unknown_is_error() {
	assert!(serde_yaml::from_str::<WatchAction>("\"deploy\"").is_err());
}

#[test]
fn watch_action_serializes_lowercase_token() {
	// `config` must emit the compose token, not the PascalCase variant name.
	assert_eq!(
		serde_yaml::to_string(&WatchAction::Sync).unwrap().trim(),
		"sync"
	);
	assert_eq!(
		serde_yaml::to_string(&WatchAction::SyncAndRestart)
			.unwrap()
			.trim(),
		"sync+restart"
	);
	assert_eq!(
		serde_yaml::to_string(&WatchAction::SyncAndExec)
			.unwrap()
			.trim(),
		"sync+exec"
	);
}

#[test]
fn watch_action_round_trips_through_config() {
	// Every variant must survive a serialize -> deserialize round-trip so
	// `config` output feeds back into podup unchanged.
	for action in [
		WatchAction::Sync,
		WatchAction::Rebuild,
		WatchAction::Restart,
		WatchAction::SyncAndRestart,
		WatchAction::SyncAndExec,
	] {
		let rendered = serde_yaml::to_string(&action).unwrap();
		let parsed: WatchAction = serde_yaml::from_str(&rendered).unwrap();
		assert_eq!(parsed, action);
	}
}

#[test]
fn watch_action_requires_target_matches_sync_family() {
	assert!(WatchAction::Sync.requires_target());
	assert!(WatchAction::SyncAndRestart.requires_target());
	assert!(WatchAction::SyncAndExec.requires_target());
	assert!(!WatchAction::Rebuild.requires_target());
	assert!(!WatchAction::Restart.requires_target());
}
