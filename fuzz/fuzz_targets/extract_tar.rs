#![no_main]

//! Fuzz the container→host tar extractor (`extract_tar_guarded`) on
//! attacker-controlled bytes. The function parses a tar that came out of a
//! (possibly compromised) container during `cp`, refusing any entry whose
//! path would escape the destination or whose mode carries group/other-write
//! or setuid/setgid/sticky bits. The unit test
//! `extract_tar_guarded_rejects_parent_traversal` covers the obvious `..`
//! escape; this target exercises every header field the fuzzer can mutate.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use libfuzzer_sys::fuzz_target;

/// Per-iteration scratch directory under `std::env::temp_dir()`, removed on
/// drop so the fuzz target never accumulates state between inputs.
struct ScratchDir {
	path: PathBuf,
}

impl ScratchDir {
	fn new() -> std::io::Result<Self> {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!(
			"podup-fuzz-extract-tar-{}-{n}",
			std::process::id(),
		));
		std::fs::create_dir_all(&path)?;
		Ok(Self { path })
	}
}

impl Drop for ScratchDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.path);
	}
}

fuzz_target!(|data: &[u8]| {
	let Ok(scratch) = ScratchDir::new() else {
		return;
	};
	// `extract_tar_guarded` returns `Result<()>`. Any `Err` is the documented
	// outcome for a hostile header (zip-slip, unreadable mode, …) and not a
	// finding; only a panic (caught by libFuzzer as a crash) would be.
	let _ = podup::fuzz_api::extract_tar_guarded(data, &scratch.path);
});
