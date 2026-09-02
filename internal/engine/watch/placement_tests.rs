use super::{
	is_dispatch_event, mark_dir_ensured, mkdir_p_argv, plan_sync_placement, validate_sync_target,
};
use crate::compose::types::{WatchAction, WatchRule};
use std::collections::HashSet;
use std::fs;
use tempfile::tempdir;

fn rule(action: WatchAction, target: Option<&str>) -> WatchRule {
	WatchRule {
		path: "src".into(),
		action,
		target: target.map(str::to_string),
		..Default::default()
	}
}

#[test]
fn dispatch_event_filters_access_and_other() {
	use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
	use notify::EventKind;
	assert!(is_dispatch_event(&EventKind::Create(CreateKind::File)));
	assert!(is_dispatch_event(&EventKind::Modify(ModifyKind::Any)));
	assert!(is_dispatch_event(&EventKind::Remove(RemoveKind::File)));
	// Access (read/open) and Other/Any must not trigger a sync.
	assert!(!is_dispatch_event(&EventKind::Access(AccessKind::Open(
		notify::event::AccessMode::Read
	))));
	assert!(!is_dispatch_event(&EventKind::Access(AccessKind::Any)));
	assert!(!is_dispatch_event(&EventKind::Other));
	assert!(!is_dispatch_event(&EventKind::Any));
}

#[test]
fn validate_sync_target_rejects_targetless_sync() {
	assert!(validate_sync_target(&rule(WatchAction::Sync, None)).is_err());
	assert!(validate_sync_target(&rule(WatchAction::SyncAndRestart, None)).is_err());
	assert!(validate_sync_target(&rule(WatchAction::SyncAndExec, None)).is_err());
}

#[test]
fn validate_sync_target_accepts_target_and_whole_container_actions() {
	assert!(validate_sync_target(&rule(WatchAction::Sync, Some("/app"))).is_ok());
	// rebuild/restart need no target.
	assert!(validate_sync_target(&rule(WatchAction::Rebuild, None)).is_ok());
	assert!(validate_sync_target(&rule(WatchAction::Restart, None)).is_ok());
}

#[test]
fn mkdir_argv_terminates_options_for_leading_dash_target() {
	// A target beginning with `-` must be passed as a path, not a flag.
	assert_eq!(mkdir_p_argv("-m0777"), vec!["mkdir", "-p", "--", "-m0777"]);
	assert_eq!(mkdir_p_argv("/app"), vec!["mkdir", "-p", "--", "/app"]);
}

#[test]
fn mark_dir_ensured_only_first_time_per_target() {
	let mut ensured: HashSet<(String, String)> = HashSet::new();
	// First time for a (container, dest) returns true (issue the mkdir)...
	assert!(mark_dir_ensured(&mut ensured, "c1", "/app"));
	// ...and subsequent calls for the same pair return false (skip it).
	assert!(!mark_dir_ensured(&mut ensured, "c1", "/app"));
	// A different container or dest is ensured independently.
	assert!(mark_dir_ensured(&mut ensured, "c2", "/app"));
	assert!(mark_dir_ensured(&mut ensured, "c1", "/other"));
}

#[test]
fn placement_directory_rule_preserves_subpath() {
	// A directory rule: a change to <root>/sub/b.txt must keep the `sub/`
	// subpath under the target directory.
	let dir = tempdir().unwrap();
	fs::create_dir(dir.path().join("sub")).unwrap();
	let changed = dir.path().join("sub/b.txt");
	fs::write(&changed, b"b").unwrap();

	let p = plan_sync_placement(dir.path(), &changed, "/app");
	assert_eq!(p.entry_name, "sub/b.txt");
	assert_eq!(p.dest_dir, "/app");
}

#[test]
fn placement_directory_rule_trailing_slash_target() {
	let dir = tempdir().unwrap();
	let changed = dir.path().join("a.txt");
	fs::write(&changed, b"a").unwrap();

	let p = plan_sync_placement(dir.path(), &changed, "/app/");
	assert_eq!(p.entry_name, "a.txt");
	assert_eq!(p.dest_dir, "/app");
}

#[test]
fn placement_single_file_rule_honours_renaming_target() {
	// A single-file rule whose target renames the file must store the entry
	// under the target basename and extract into the target's parent.
	let dir = tempdir().unwrap();
	let src = dir.path().join("settings.yml");
	fs::write(&src, b"k: v").unwrap();

	let p = plan_sync_placement(&src, &src, "/app/config.yml");
	assert_eq!(p.entry_name, "config.yml");
	assert_eq!(p.dest_dir, "/app");
}

#[test]
fn placement_single_file_rule_same_basename() {
	// The existing same-basename case still lands the file at the target.
	let dir = tempdir().unwrap();
	let src = dir.path().join("app.txt");
	fs::write(&src, b"x").unwrap();

	let p = plan_sync_placement(&src, &src, "/newdir/app.txt");
	assert_eq!(p.entry_name, "app.txt");
	assert_eq!(p.dest_dir, "/newdir");
}

#[test]
fn placement_single_file_rule_target_at_root() {
	let dir = tempdir().unwrap();
	let src = dir.path().join("app.txt");
	fs::write(&src, b"x").unwrap();

	let p = plan_sync_placement(&src, &src, "/app.txt");
	assert_eq!(p.entry_name, "app.txt");
	assert_eq!(p.dest_dir, "/");
}
