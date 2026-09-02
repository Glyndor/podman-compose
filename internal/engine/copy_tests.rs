use super::{
	copy_landed, join_archive_path, parse_endpoint, uploaded_entry_size, ExpectedEntry, PathStat,
};

#[test]
fn join_archive_path_does_not_double_the_separator() {
	// The #1097 re-verify stats `<dir>/<entry>`; a dir already ending in `/`
	// (notably root) must not produce `//entry`, which libpod reads as a
	// different path and 404s, turning a landed copy into a false failure.
	assert_eq!(join_archive_path("/tmp", "f.txt"), "/tmp/f.txt");
	assert_eq!(join_archive_path("/tmp/", "f.txt"), "/tmp/f.txt");
	assert_eq!(join_archive_path("/", "f.txt"), "/f.txt");
}

#[test]
fn copy_landed_asks_whether_the_entry_matches_what_was_uploaded() {
	let want = ExpectedEntry { size: 42 };
	let stat = |size: u64| PathStat {
		size,
		..PathStat::default()
	};
	// The entry is there and is the size that was sent -> landed.
	assert!(copy_landed(&want, Some(&stat(42))));
	// A failed PUT leaves the old entry, which is a different size.
	assert!(!copy_landed(&want, Some(&stat(41))));
	assert!(!copy_landed(&want, Some(&stat(0))));
	// The entry vanished, or never appeared.
	assert!(!copy_landed(&want, None));
}

/// The case the previous signal could not express, and the reason it
/// changed: copying the **same** file twice.
///
/// The old check required the destination's mtime to move. The archive sets
/// that mtime from the source, so re-copying an unchanged file leaves it
/// identical by construction — no resolution would have helped — and the
/// second copy was reported as a failure. Matching against what was uploaded
/// answers correctly.
#[test]
fn copying_an_unchanged_file_twice_is_confirmed() {
	let want = ExpectedEntry { size: 42 };
	let already_there = PathStat {
		size: 42,
		..PathStat::default()
	};
	assert!(copy_landed(&want, Some(&already_there)));
}

/// The size that goes into the comparison is the source file's real length,
/// and a directory has none.
///
/// A mutation replacing the length with a constant survived every other test
/// here, because they all build `ExpectedEntry` by hand — this is the only
/// one that goes through the filesystem.
#[test]
fn the_expected_size_comes_from_the_source_file() {
	let dir = tempfile::tempdir().unwrap();
	let file = dir.path().join("payload.bin");
	std::fs::write(&file, vec![7u8; 1234]).unwrap();
	assert_eq!(uploaded_entry_size(&file), Some(1234));

	std::fs::write(&file, b"").unwrap();
	assert_eq!(
		uploaded_entry_size(&file),
		Some(0),
		"an empty file has a size"
	);

	// A directory upload has nothing comparable, so it stays unverifiable
	// and fail-closed rather than confirming on the directory's own size.
	assert_eq!(uploaded_entry_size(dir.path()), None);
	assert_eq!(uploaded_entry_size(&dir.path().join("absent")), None);
}

/// Two copies inside one second, which is what #1270 measured on Podman 6:
/// the mtime string is identical either side of the PUT because the runtime
/// reports whole seconds, while the size moved. Under the old signal this
/// was three failures in six back-to-back copies.
#[test]
fn two_copies_in_the_same_second_are_told_apart_by_size() {
	let same_second = "2026-08-03T18:36:05Z";
	let before = PathStat {
		size: 14,
		mtime: same_second.into(),
		..PathStat::default()
	};
	let after = PathStat {
		size: 15,
		mtime: same_second.into(),
		..PathStat::default()
	};
	assert_eq!(before.mtime, after.mtime, "the fixture must share an mtime");
	// What was uploaded is the 15-byte version.
	assert!(copy_landed(&ExpectedEntry { size: 15 }, Some(&after)));
	// And the pre-PUT entry would not have satisfied it.
	assert!(!copy_landed(&ExpectedEntry { size: 15 }, Some(&before)));
}

#[test]
fn parse_service_colon_path() {
	assert_eq!(parse_endpoint("web:/app/data"), Some(("web", "/app/data")));
}

#[test]
fn parse_local_path_no_colon() {
	assert_eq!(parse_endpoint("/tmp/file.txt"), None);
}

#[test]
fn parse_dash_is_local() {
	assert_eq!(parse_endpoint("-"), None);
}

#[cfg(windows)]
#[test]
fn parse_windows_drive_letter_is_local() {
	assert_eq!(parse_endpoint("C:\\Users\\foo"), None);
}

#[cfg(not(windows))]
#[test]
fn single_char_service_parses_on_unix() {
	// On Unix a one-character service name is valid; only Windows treats a
	// single-char prefix as a drive letter.
	assert_eq!(parse_endpoint("c:/tmp/file"), Some(("c", "/tmp/file")));
	assert_eq!(parse_endpoint("w:data"), Some(("w", "data")));
}

#[test]
fn parse_empty_service_or_path() {
	assert_eq!(parse_endpoint(":path"), None);
	assert_eq!(parse_endpoint("svc:"), None);
}

#[cfg(windows)]
#[test]
fn parse_windows_drive_letter_forward_slash() {
	assert_eq!(parse_endpoint("C:/Users/foo"), None);
}

#[test]
fn parse_service_with_relative_path() {
	assert_eq!(
		parse_endpoint("web:data/file.txt"),
		Some(("web", "data/file.txt"))
	);
}

#[test]
fn parse_service_name_with_dots() {
	assert_eq!(
		parse_endpoint("my.service:/app/config"),
		Some(("my.service", "/app/config"))
	);
}

#[test]
fn check_endpoint_rejects_dash() {
	let err = super::check_endpoint("-").unwrap_err();
	assert!(format!("{err}").contains("stdin/stdout"), "got: {err}");
}

#[test]
fn check_endpoint_rejects_empty_container_path() {
	let err = super::check_endpoint("web:").unwrap_err();
	assert!(
		format!("{err}").contains("empty container path"),
		"got: {err}"
	);
}

#[test]
fn check_endpoint_allows_normal_forms() {
	// A plain local path, a proper SERVICE:PATH, and a relative host path are
	// all fine (validation only rejects `-` and `SERVICE:`).
	assert!(super::check_endpoint("/tmp/file").is_ok());
	assert!(super::check_endpoint("web:/app/data").is_ok());
	assert!(super::check_endpoint("./local").is_ok());
}
