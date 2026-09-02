use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// In-memory release source signed with a throwaway key, so the full flow
/// (version gate, signature, checksum, install) runs without network or the
/// placeholder pubkey guard.
struct MockSource {
	latest: String,
	assets: HashMap<String, Vec<u8>>,
	fetched: RefCell<Vec<String>>,
}

impl ReleaseSource for MockSource {
	fn latest_version(&self) -> crate::Result<String> {
		Ok(self.latest.clone())
	}
	fn fetch(&self, asset: &str) -> crate::Result<Vec<u8>> {
		self.fetched.borrow_mut().push(asset.to_string());
		self.assets
			.get(asset)
			.cloned()
			.ok_or_else(|| ComposeError::Update(format!("missing asset {asset}")))
	}
}

#[test]
fn up_to_date_skips_download() {
	let src = MockSource {
		latest: "v0.6.0".into(),
		assets: HashMap::new(),
		fetched: RefCell::new(Vec::new()),
	};
	run_with(&src, "0.6.0", UpdateOptions::default()).unwrap();
	assert!(
		src.fetched.borrow().is_empty(),
		"must not fetch when current"
	);
}

#[test]
fn newer_release_check_only_does_not_install() {
	let src = MockSource {
		latest: "v0.7.0".into(),
		assets: HashMap::new(),
		fetched: RefCell::new(Vec::new()),
	};
	let opts = UpdateOptions {
		check_only: true,
		force: false,
	};
	run_with(&src, "0.6.0", opts).unwrap();
	assert!(
		src.fetched.borrow().is_empty(),
		"check-only must not download"
	);
}

#[test]
fn bad_version_from_source_errors() {
	let src = MockSource {
		latest: "not-a-version".into(),
		assets: HashMap::new(),
		fetched: RefCell::new(Vec::new()),
	};
	assert!(run_with(&src, "0.6.0", UpdateOptions::default()).is_err());
}

#[test]
fn newer_release_with_real_install_path() {
	// Only meaningful where the host maps to a known release asset.
	let Some(asset) = install::platform_asset() else {
		return;
	};

	use ed25519_dalek::{Signer, SigningKey};
	let sk = SigningKey::from_bytes(&[42u8; 32]);

	let binary = b"the new podup binary".to_vec();
	let digest = verify::sha256_hex(&binary);
	let sums = format!("{digest}  {asset}\n");
	let sig = sk.sign(sums.as_bytes()).to_bytes().to_vec();

	let mut assets = HashMap::new();
	assets.insert(asset.to_string(), binary.clone());
	assets.insert("SHA256SUMS".to_string(), sums.into_bytes());
	assets.insert("SHA256SUMS.sig".to_string(), sig);

	let src = MockSource {
		latest: "v9.9.9".into(),
		assets,
		fetched: RefCell::new(Vec::new()),
	};

	// The manifest is internally consistent but signed with a throwaway key,
	// not the embedded release key — verification must fail closed, proving
	// the gate rejects anything not signed by the real key.
	let err = run_with(&src, "0.6.0", UpdateOptions::default()).unwrap_err();
	assert!(matches!(err, ComposeError::Update(_)));
	assert!(src.fetched.borrow().contains(&"SHA256SUMS.sig".to_string()));
}

#[test]
fn package_managed_binary_refuses_and_does_not_download() {
	// A newer release is available and --force is set, so the flow reaches the
	// package-manager guard. With a manager owning the binary it must refuse
	// before fetching anything.
	let src = MockSource {
		latest: "v9.9.9".into(),
		assets: HashMap::new(),
		fetched: RefCell::new(Vec::new()),
	};
	let opts = UpdateOptions {
		check_only: false,
		force: true,
	};
	let err = run_with_guard(&src, "0.6.0", opts, Some("apt")).unwrap_err();
	match err {
		ComposeError::Update(msg) => assert!(msg.contains("apt")),
		other => panic!("expected Update error, got {other:?}"),
	}
	assert!(
		src.fetched.borrow().is_empty(),
		"a package-managed binary must not download an update"
	);
}

#[test]
fn force_reinstall_same_version_check_only_announces_reinstall() {
	// `--force --check` on the *same* version takes the reinstall branch (not
	// "up to date") and stops at the check-only gate without downloading.
	let src = MockSource {
		latest: "v0.6.0".into(),
		assets: HashMap::new(),
		fetched: RefCell::new(Vec::new()),
	};
	let opts = UpdateOptions {
		check_only: true,
		force: true,
	};
	run_with_guard(&src, "0.6.0", opts, None).unwrap();
	assert!(
		src.fetched.borrow().is_empty(),
		"check-only must not download even when forcing a reinstall"
	);
}

#[test]
fn check_only_returns_before_package_manager_guard() {
	// --check must short-circuit even when a package manager owns the binary,
	// so `podup update --check` never errors on a deb install.
	let src = MockSource {
		latest: "v9.9.9".into(),
		assets: HashMap::new(),
		fetched: RefCell::new(Vec::new()),
	};
	let opts = UpdateOptions {
		check_only: true,
		force: false,
	};
	run_with_guard(&src, "0.6.0", opts, Some("apt")).unwrap();
	assert!(src.fetched.borrow().is_empty());
}

#[test]
fn exit_code_is_off_claps_reserved_two() {
	// clap returns 2 for usage errors; the update-failure code must stay
	// distinct so scripts can tell them apart.
	assert_eq!(
		exit_code(&ComposeError::Update("x".into())),
		UPDATE_FAILURE_EXIT_CODE
	);
	assert_ne!(UPDATE_FAILURE_EXIT_CODE, 2);
}
