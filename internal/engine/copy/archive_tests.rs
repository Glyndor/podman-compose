use std::path::Path;

#[test]
fn pack_path_single_file() {
	let dir = tempfile::tempdir().expect("tempdir");
	let file = dir.path().join("data.txt");
	std::fs::write(&file, b"hello").expect("write");
	let result = super::pack_path(&file, false, None);
	assert!(result.is_ok());
	let bytes = result.unwrap();
	assert!(!bytes.is_empty());
}

#[test]
fn pack_path_directory() {
	let dir = tempfile::tempdir().expect("tempdir");
	let subdir = dir.path().join("mydir");
	std::fs::create_dir(&subdir).expect("mkdir");
	std::fs::write(subdir.join("a.txt"), b"aaa").expect("write");
	std::fs::write(subdir.join("b.txt"), b"bbb").expect("write");
	let result = super::pack_path(&subdir, false, None);
	assert!(result.is_ok());
	assert!(!result.unwrap().is_empty());
}

#[test]
fn pack_path_missing_source_is_a_cp_error() {
	// A missing host source on `cp` must read as a cp error, not a build error.
	let missing = std::path::Path::new("/nonexistent-host-source-xyz");
	let err = super::pack_path(missing, false, None).unwrap_err();
	let msg = err.to_string();
	assert!(msg.contains("cp error"), "wrong category: {msg:?}");
	assert!(
		!msg.contains("build error"),
		"must not be a build error: {msg:?}"
	);
}

/// Build an uncompressed tar archive with a single entry at `path`. The name
/// is written straight into the GNU header so a hostile `..` path can be
/// forged (the safe `set_path`/`append_data` helpers reject `..`).
fn tar_bytes_with(path: &str, data: &[u8]) -> Vec<u8> {
	let mut header = tar::Header::new_gnu();
	header.set_size(data.len() as u64);
	header.set_mode(0o644);
	header.set_entry_type(tar::EntryType::Regular);
	let name = path.as_bytes();
	header.as_gnu_mut().expect("gnu header").name[..name.len()].copy_from_slice(name);
	header.set_cksum();
	let mut builder = tar::Builder::new(Vec::new());
	builder.append(&header, data).expect("append");
	builder.into_inner().expect("finish")
}

#[test]
fn extract_tar_guarded_writes_benign_entry() {
	let dir = tempfile::tempdir().expect("tempdir");
	let bytes = tar_bytes_with("hello.txt", b"hi");
	super::extract_tar_guarded(&bytes[..], dir.path()).expect("extract");
	assert_eq!(
		std::fs::read(dir.path().join("hello.txt")).expect("read"),
		b"hi"
	);
}

#[test]
fn extract_archive_to_file_honors_user_filename() {
	// dst is NOT a dir: the single entry's content must land at exactly `dst`,
	// ignoring the daemon-supplied entry name (a hostile image must not pick
	// the on-host filename).
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("myname.txt");
	let bytes = tar_bytes_with("evil-name", b"payload");
	super::extract_archive(&bytes, &dst).expect("extract");
	assert_eq!(std::fs::read(&dst).expect("read"), b"payload");
	assert!(
		!dir.path().join("evil-name").exists(),
		"daemon entry name must not be used as the on-host filename"
	);
}

#[test]
fn extract_archive_to_file_rejects_multiple_entries() {
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("out.txt");
	let mut builder = tar::Builder::new(Vec::new());
	for n in ["a.txt", "b.txt"] {
		let mut h = tar::Header::new_gnu();
		h.set_size(1);
		h.set_mode(0o644);
		h.set_entry_type(tar::EntryType::Regular);
		h.set_path(n).expect("path");
		h.set_cksum();
		builder.append(&h, &b"x"[..]).expect("append");
	}
	let bytes = builder.into_inner().expect("finish");
	assert!(super::extract_archive(&bytes, &dst).is_err());
}

/// Build a directory archive shaped like libpod's `cp container:/srcdir`:
/// a wrapper directory entry plus its children, all under the basename.
fn tar_dir_with(wrapper: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
	let mut builder = tar::Builder::new(Vec::new());
	let mut d = tar::Header::new_gnu();
	d.set_size(0);
	d.set_mode(0o755);
	d.set_entry_type(tar::EntryType::Directory);
	d.set_path(format!("{wrapper}/")).expect("path");
	d.set_cksum();
	builder.append(&d, std::io::empty()).expect("dir");
	for (name, data) in files {
		let mut h = tar::Header::new_gnu();
		h.set_size(data.len() as u64);
		h.set_mode(0o644);
		h.set_entry_type(tar::EntryType::Regular);
		h.set_path(format!("{wrapper}/{name}")).expect("path");
		h.set_cksum();
		builder.append(&h, *data).expect("file");
	}
	builder.into_inner().expect("finish")
}

#[test]
fn extract_dir_into_missing_dest_creates_and_flattens() {
	// dst does not exist and the source is a directory: dst is created and the
	// source's *contents* land directly in it (the wrapper level is collapsed),
	// matching `docker cp`.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("newdir");
	let bytes = tar_dir_with("srcdir", &[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
	super::extract_archive(&bytes, &dst).expect("extract");
	assert!(dst.is_dir());
	assert_eq!(std::fs::read(dst.join("a.txt")).expect("read"), b"aaa");
	assert_eq!(std::fs::read(dst.join("b.txt")).expect("read"), b"bbb");
	assert!(
		!dst.join("srcdir").exists(),
		"the wrapper directory level must be collapsed"
	);
}

#[test]
fn extract_single_file_into_missing_dest_still_writes_exact_name() {
	// The single-file path is unchanged: content at exactly `dst`.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("renamed.txt");
	let bytes = tar_bytes_with("original.txt", b"data");
	super::extract_archive(&bytes, &dst).expect("extract");
	assert_eq!(std::fs::read(&dst).expect("read"), b"data");
}

#[cfg(unix)]
#[test]
fn extract_strips_group_other_write_and_special_bits() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().expect("tempdir");
	// World-writable + setuid entry from an untrusted container.
	let mut h = tar::Header::new_gnu();
	h.set_size(2);
	h.set_mode(0o4777);
	h.set_entry_type(tar::EntryType::Regular);
	h.set_path("f").expect("path");
	h.set_cksum();
	let mut builder = tar::Builder::new(Vec::new());
	builder.append(&h, &b"hi"[..]).expect("append");
	let bytes = builder.into_inner().expect("finish");
	super::extract_tar_guarded(&bytes[..], dir.path()).expect("extract");
	let mode = std::fs::metadata(dir.path().join("f"))
		.expect("meta")
		.permissions()
		.mode()
		& 0o7777;
	assert_eq!(mode & 0o022, 0, "group/other write must be stripped");
	assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky must be stripped");
}

#[test]
fn extract_tar_guarded_rejects_parent_traversal() {
	// A compromised container can return a tar whose entry escapes the
	// destination via `..`; the guard must refuse it and write nothing.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("dest");
	std::fs::create_dir(&dst).expect("mkdir");
	let bytes = tar_bytes_with("../evil.txt", b"pwned");
	let err = super::extract_tar_guarded(&bytes[..], &dst).unwrap_err();
	assert!(
		format!("{err}").contains("escapes destination"),
		"expected a zip-slip refusal, got: {err}"
	);
	assert!(
		!dir.path().join("evil.txt").exists(),
		"traversal entry must not be written outside the destination"
	);
}

#[test]
fn extract_archive_to_file_rejects_empty_archive() {
	// An empty tar (no entries) against a file destination is an error: there
	// is nothing to write to `dst`.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("out.txt");
	let bytes = tar::Builder::new(Vec::new()).into_inner().expect("finish");
	let err = super::extract_archive(&bytes, &dst).unwrap_err();
	assert!(format!("{err}").contains("empty"), "got: {err}");
}

#[test]
fn extract_archive_dir_source_creates_non_existent_dest() {
	// A directory source against a non-existent destination creates the
	// destination directory (matching `docker cp`) rather than erroring,
	// regardless of the destination's name.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("out.txt");
	let mut h = tar::Header::new_gnu();
	h.set_size(0);
	h.set_mode(0o755);
	h.set_entry_type(tar::EntryType::Directory);
	h.set_path("subdir/").expect("path");
	h.set_cksum();
	let mut builder = tar::Builder::new(Vec::new());
	builder.append(&h, std::io::empty()).expect("append");
	let bytes = builder.into_inner().expect("finish");
	super::extract_archive(&bytes, &dst).expect("extract");
	assert!(dst.is_dir());
}

#[test]
fn extract_archive_to_file_errors_on_missing_parent() {
	// docker/podman `cp` error when the destination's parent directory does not
	// exist, rather than silently creating the whole chain.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("new").join("nested").join("file.txt");
	let bytes = tar_bytes_with("ignored-name", b"data");
	let err = super::extract_archive(&bytes, &dst).unwrap_err();
	assert!(format!("{err}").contains("no such directory"), "got: {err}");
	assert!(!dst.exists(), "nothing must be created on a missing parent");
	assert!(
		!dst.parent().unwrap().exists(),
		"the parent chain must not be created"
	);
}

#[test]
fn extract_archive_to_trailing_slash_missing_dir_errors() {
	// A trailing-slash destination names a directory; when it does not exist,
	// `cp` errors instead of hitting a misleading "Is a directory" (EISDIR).
	let dir = tempfile::tempdir().expect("tempdir");
	// Keep the trailing separator on the path string.
	let dst = std::path::PathBuf::from(format!("{}/newdir/", dir.path().display()));
	let bytes = tar_bytes_with("hostname", b"data");
	let err = super::extract_archive(&bytes, &dst).unwrap_err();
	assert!(format!("{err}").contains("no such directory"), "got: {err}");
	assert!(!dst.exists(), "nothing must be created");
}

#[test]
fn extract_archive_dir_source_onto_existing_file_errors() {
	// A directory source whose destination already exists as a regular file is
	// a clear error, not a misleading "File exists" (EEXIST); the file is left
	// untouched.
	let dir = tempfile::tempdir().expect("tempdir");
	let dst = dir.path().join("existing");
	std::fs::write(&dst, b"keep").expect("write");
	let bytes = tar_dir_with("srcdir", &[("a.txt", b"aaa")]);
	let err = super::extract_archive(&bytes, &dst).unwrap_err();
	assert!(
		format!("{err}").contains("cannot copy a directory"),
		"got: {err}"
	);
	assert_eq!(
		std::fs::read(&dst).expect("read"),
		b"keep",
		"the existing file must be left untouched"
	);
}

#[test]
fn unpacked_path_mirrors_unpack_in_and_never_leaves_the_destination() {
	// `unpack_in` drops every non-Normal component, so the chmod target must
	// be rebuilt the same way. `dst.join(rel)` cannot be used: joining an
	// absolute path discards the base, which is how a hostile entry name
	// would have redirected the chmod onto a real host file.
	let dst = Path::new("/tmp/dest");
	assert_eq!(
		super::unpacked_path(dst, Path::new("/etc/passwd")),
		Some(std::path::PathBuf::from("/tmp/dest/etc/passwd"))
	);
	assert_eq!(
		super::unpacked_path(dst, Path::new("../../home/u/.ssh/id_ed25519")),
		Some(std::path::PathBuf::from("/tmp/dest/home/u/.ssh/id_ed25519"))
	);
	assert_eq!(
		super::unpacked_path(dst, Path::new("a/b.txt")),
		Some(std::path::PathBuf::from("/tmp/dest/a/b.txt"))
	);
	assert_eq!(super::unpacked_path(dst, Path::new("/")), None);
	assert_eq!(super::unpacked_path(dst, Path::new("..")), None);
}

#[cfg(unix)]
#[test]
fn absolute_entry_name_cannot_chmod_a_file_outside_the_destination() {
	use std::os::unix::fs::PermissionsExt;

	// The victim lives outside the destination and starts private.
	let dir = tempfile::tempdir().expect("tempdir");
	let victim = dir.path().join("secret");
	std::fs::write(&victim, b"private").expect("write");
	std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).expect("chmod");

	let dst = dir.path().join("dest");
	std::fs::create_dir(&dst).expect("mkdir");

	// Build an entry whose stored name is the victim's ABSOLUTE path. The
	// safe setter refuses absolute paths, but a hostile archive is not built
	// with the safe setter, so write the header's name field directly — this
	// is the shape the guard has to survive.
	let target = victim.to_str().expect("utf-8");
	let mut header = tar::Header::new_gnu();
	header.set_size(4);
	header.set_mode(0o644);
	header.set_entry_type(tar::EntryType::Regular);
	{
		let name = &mut header.as_gnu_mut().expect("gnu header").name;
		name.iter_mut().for_each(|b| *b = 0);
		name[..target.len()].copy_from_slice(target.as_bytes());
	}
	header.set_cksum();
	let mut builder = tar::Builder::new(Vec::new());
	builder.append(&header, &b"evil"[..]).expect("append");
	let bytes = builder.into_inner().expect("tar");

	super::extract_tar_guarded(&bytes[..], &dst).expect("extraction should succeed");

	let mode = std::fs::metadata(&victim)
		.expect("stat")
		.permissions()
		.mode()
		& 0o777;
	assert_eq!(
		mode, 0o600,
		"a file outside the destination must keep its mode: dst.join(absolute) discards the base"
	);
	assert_eq!(std::fs::read(&victim).expect("read"), b"private");
}
