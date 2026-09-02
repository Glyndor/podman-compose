use super::walk_dir;
use std::path::PathBuf;

#[test]
fn walk_dir_returns_every_entry_in_sorted_order() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::create_dir_all(dir.path().join("sub")).unwrap();
	std::fs::write(dir.path().join("a"), b"a").unwrap();
	std::fs::write(dir.path().join("sub/b"), b"b").unwrap();
	let got: Vec<PathBuf> = walk_dir(dir.path()).unwrap();
	let names: Vec<String> = got
		.iter()
		.map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
		.collect();
	assert_eq!(names, vec!["a", "sub", "b"]);
}
