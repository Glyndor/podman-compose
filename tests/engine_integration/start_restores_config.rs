//! The premise `autostart --mode start` rests on: Podman's store survives a
//! stop, so starting a container restores the configuration it was created
//! with, and the boot path needs no compose file to put it back.
//!
//! That claim was believed rather than measured. It is asserted here instead of
//! written down because the podman lane runs this file against Podman 5 and 6
//! (`podman-lane.yml`, `podman: ["5", "6"]`), so it is re-answered on both
//! majors on every pull request. A reading taken once on one machine is a note
//! that rots; this is a control.
//!
//! The field list is not invented. It comes from diffing a real deployment's
//! `HostConfig` against a bare `podman run` container, so every key below is
//! measured as non-default rather than remembered as interesting. Four of them
//! would not have been guessed: `MemorySwappiness` is written by Podman when a
//! memory limit is present and never appears in the compose file, `NetworkMode`
//! flips to `bridge` merely because a network is declared, `Init` is *absent*
//! from a default container rather than false (so a diff visiting only shared
//! keys would skip it), and `Ulimits` carries a host default alongside the
//! declared one, which is why the assertion below looks up the named limit
//! instead of comparing the array.
use super::*;

/// Every `HostConfig` key the reference deployment sets away from the default.
/// A container that comes back with all of these intact has kept the settings
/// an operator would notice losing.
const HOST_CONFIG_KEYS: &[&str] = &[
	"Binds",
	"CapDrop",
	"Init",
	"LogConfig",
	"Memory",
	"MemorySwap",
	"MemorySwappiness",
	"NetworkMode",
	"PidsLimit",
	"PortBindings",
	"ReadonlyRootfs",
	"RestartPolicy",
	"SecurityOpt",
	"ShmSize",
	"Tmpfs",
];

async fn host_config(client: &Client, name: &str) -> serde_json::Value {
	let path = format!("/v5.0.0/libpod/containers/{name}/json");
	let v: serde_json::Value = client.get_json(&path).await.expect("inspect");
	v.get("HostConfig").cloned().expect("HostConfig present")
}

/// The soft limit of a named ulimit, or `None`. Looked up by name because the
/// array also carries the host's own `RLIMIT_NPROC`, whose value is
/// machine-specific: comparing the whole array would assert the runner's
/// configuration rather than the container's.
fn ulimit(host_config: &serde_json::Value, name: &str) -> Option<i64> {
	host_config
		.get("Ulimits")?
		.as_array()?
		.iter()
		.find(|u| u.get("Name").and_then(|n| n.as_str()) == Some(name))?
		.get("Soft")?
		.as_i64()
}

#[tokio::test]
async fn a_stopped_container_starts_back_with_its_configuration() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	// A second connection for the raw inspects: `Client` is not `Clone`, and the
	// engine takes ownership of the first.
	let inspector = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("startcfg");
	let engine = Engine::new(client, proj.clone());
	// Exercises what the reference deployment exercises: memory and swap, pids,
	// shm, a named ulimit, read-only rootfs, dropped capabilities, security_opt,
	// a port bound to an explicit host IP, a named volume, tmpfs with options,
	// restart policy, a sized log driver, and init.
	let file = parse_str(&format!(
		"services:\n  \
		 web:\n    \
		 image: alpine:latest\n    \
		 command: [\"sleep\", \"infinity\"]\n    \
		 init: true\n    \
		 read_only: true\n    \
		 mem_limit: 512m\n    \
		 pids_limit: 256\n    \
		 shm_size: 16m\n    \
		 restart: unless-stopped\n    \
		 cap_drop: [CHOWN, SETUID]\n    \
		 security_opt: [\"no-new-privileges\"]\n    \
		 ulimits:\n      \
		 nofile:\n        soft: 4096\n        hard: 8192\n    \
		 ports:\n      - \"127.0.0.1:0:8080\"\n    \
		 tmpfs:\n      - /run:size=8m,mode=755\n    \
		 logging:\n      driver: json-file\n      options:\n        max-size: 10m\n    \
		 volumes:\n      - {proj}-cache:/cache\nvolumes:\n  {proj}-cache:\n"
	))
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	let before = host_config(&inspector, &cname).await;

	// Guard the guard. If the compose keys above stopped reaching Podman, every
	// key would read the same trivially on both sides and the test would pass
	// having compared nothing. Assert the settings arrived at all first.
	assert_eq!(
		before.get("Memory").and_then(|v| v.as_i64()),
		Some(536870912)
	);
	assert_eq!(before.get("PidsLimit").and_then(|v| v.as_i64()), Some(256));
	assert_eq!(
		before.get("ReadonlyRootfs").and_then(|v| v.as_bool()),
		Some(true)
	);
	assert_eq!(ulimit(&before, "RLIMIT_NOFILE"), Some(4096));

	engine.stop(&file, &[]).await.unwrap();
	engine.start(&file, &[]).await.unwrap();
	let after = host_config(&inspector, &cname).await;

	for key in HOST_CONFIG_KEYS {
		assert_eq!(
			before.get(*key),
			after.get(*key),
			"HostConfig.{key} changed across stop/start:\n  before {:?}\n  after  {:?}",
			before.get(*key),
			after.get(*key)
		);
	}
	assert_eq!(
		ulimit(&before, "RLIMIT_NOFILE"),
		ulimit(&after, "RLIMIT_NOFILE"),
		"the declared ulimit must survive"
	);

	engine.down_with_options(&file, true).await.unwrap();
}
