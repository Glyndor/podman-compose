//! Tests for #1736: `cp` must not follow a symlink planted inside the
//! archive being extracted, or rename the contents of an out-of-archive
//! directory into the destination.
//!
//! The flat `cp svc:/` archive libpod returns for a container whose source
//! is a directory can carry an entry that is itself a symlink pointing
//! outside. After `extract_tar_guarded` lands the bytes, the destination
//! holds that symlink as its only child. The flatten step then ran
//! `is_dir()` on the child, which followed the symlink, called
//! `read_dir` on the target, and renamed each file out of it: an
//! out-of-archive directory was emptied into the destination. The fix
//! uses `symlink_metadata` so the symlink stays a symlink.

#[cfg(unix)]
#[test]
fn extract_archive_does_not_empty_victim_through_planted_symlink() {
	// Victim lives outside the destination and holds private material.
	let dir = tempfile::tempdir().expect("tempdir");
	let victim = dir.path().join("victim");
	std::fs::create_dir(&victim).expect("mkdir");
	std::fs::write(victim.join("id_ed25519"), b"key").expect("write");
	std::fs::write(victim.join("known_hosts"), b"hosts").expect("write");

	let dst = dir.path().join("dst");

	// Build the archive shape that triggers the defect: a single
	// directory entry naming the destination itself (so
	// `archive_contains_dir` reports true and `extract_archive` reaches
	// the flatten path), followed by a single symlink entry whose name
	// has no parent and whose target points at the victim. After
	// `extract_tar_guarded` lands the bytes, `dst` holds exactly one
	// child: the symlink.
	let mut builder = tar::Builder::new(Vec::new());
	let mut d = tar::Header::new_gnu();
	d.set_size(0);
	d.set_mode(0o755);
	d.set_entry_type(tar::EntryType::Directory);
	d.set_path("./").expect("path");
	d.set_cksum();
	builder.append(&d, std::io::empty()).expect("dir");
	let mut s = tar::Header::new_gnu();
	s.set_size(0);
	s.set_mode(0o755);
	s.set_entry_type(tar::EntryType::Symlink);
	s.set_path("w").expect("path");
	// The symlink target has to land in the GNU linkname field; the safe
	// `set_link_name` helper accepts the value, but writing the header
	// field directly matches what the production extractor has to
	// survive (an archive a hostile container could craft).
	let target = victim.to_str().expect("utf-8");
	{
		let gnu = s.as_gnu_mut().expect("gnu header");
		let name = &mut gnu.linkname;
		name.iter_mut().for_each(|b| *b = 0);
		let bytes = target.as_bytes();
		name[..bytes.len()].copy_from_slice(bytes);
	}
	s.set_cksum();
	builder.append(&s, std::io::empty()).expect("symlink");
	let bytes = builder.into_inner().expect("finish");

	// The call's return value is irrelevant; the safety invariant is that
	// nothing in the victim directory has moved, regardless of whether the
	// extraction succeeded or errored out.
	let _ = super::extract_archive(&bytes, &dst);

	assert!(
		victim.join("id_ed25519").exists(),
		"id_ed25519 was moved out of the victim directory through a symlink planted in the archive"
	);
	assert_eq!(
		std::fs::read(victim.join("id_ed25519")).expect("read"),
		b"key"
	);
	assert!(
		victim.join("known_hosts").exists(),
		"known_hosts was moved out of the victim directory through a symlink planted in the archive"
	);
	assert_eq!(
		std::fs::read(victim.join("known_hosts")).expect("read"),
		b"hosts"
	);
}
