use std::os::unix::fs::PermissionsExt;

use super::{write_units, QuadletUnit};

fn unit(filename: &str, owner: &str) -> QuadletUnit {
	QuadletUnit {
		filename: filename.to_string(),
		contents: format!("# podup-owner: {owner}\n[Container]\nEnvironment=PGPASSWORD=hunter2\n"),
	}
}

#[test]
fn refuses_to_overwrite_a_sibling_projects_unit() {
	// `app` + service `extra-web` and `app-extra` + service `web` collide on
	// one filename. Overwriting would also re-stamp the marker, so the next
	// uninstall of the wrong project would delete the survivor.
	let dir = tempfile::tempdir().expect("tempdir");
	write_units(dir.path(), &[unit("app-extra-web.container", "app-extra")]).expect("first");

	let err = write_units(dir.path(), &[unit("app-extra-web.container", "app")])
		.expect_err("must refuse");
	assert!(
		format!("{err}").contains("belongs to project 'app-extra'"),
		"got: {err}"
	);

	let kept = std::fs::read_to_string(dir.path().join("app-extra-web.container")).expect("read");
	assert!(
		kept.contains("# podup-owner: app-extra"),
		"the original owner's marker must survive"
	);
}

#[test]
fn rewriting_your_own_unit_is_allowed() {
	let dir = tempfile::tempdir().expect("tempdir");
	write_units(dir.path(), &[unit("app-web.container", "app")]).expect("first");
	write_units(dir.path(), &[unit("app-web.container", "app")]).expect("second");
}

#[test]
fn units_are_written_private_because_they_carry_environment_values() {
	let dir = tempfile::tempdir().expect("tempdir");
	let written = write_units(dir.path(), &[unit("app-web.container", "app")]).expect("write");
	let mode = std::fs::metadata(&written[0])
		.expect("stat")
		.permissions()
		.mode()
		& 0o777;
	assert_eq!(
		mode, 0o600,
		"a unit holding Environment= secrets must not be world-readable"
	);
}

#[test]
fn an_existing_unit_written_by_an_older_podup_is_tightened() {
	let dir = tempfile::tempdir().expect("tempdir");
	let path = dir.path().join("app-web.container");
	std::fs::write(&path, "[Container]\n").expect("seed");
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

	write_units(dir.path(), &[unit("app-web.container", "app")]).expect("write");

	let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
	assert_eq!(
		mode, 0o600,
		"re-installing must tighten a unit left loose by an older version"
	);
}
