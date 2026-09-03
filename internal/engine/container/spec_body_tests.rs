//! Field-by-field assertion that the `SpecGenerator` literal `Engine::create_and_start`
//! builds is sent over the wire to `/containers/create` exactly as the compose
//! fields read.
//!
//! `cargo mutants` once deleted 60 of the literal's fields one at a time and
//! the unit suite stayed green for every one. The live Podman lane was the
//! only thing that noticed a dropped field, and even then only when a scenario
//! happened to exercise the field's effect. This test pins every compose key
//! that maps onto the literal onto the exact JSON value the engine transmits,
//! against the bytes the fake socket receives, so a mutation that drops or
//! renames any one field fails this assertion on the spot.

use super::Engine;
use crate::engine::fake_podman;

/// The fake socket must answer three things the engine calls beyond
/// `/containers/create`: a `GET /containers/json` listing (returns empty so
/// every service is treated as new), a pre-create `DELETE /containers/{name}`
/// (404 is fine, the engine ignores it), and, for any inline
/// `content:`/`file:` secret, the per-name inspect/delete/create dance. The
/// test below uses only `external: true` secrets, so the inspect-then-create
/// branch for owned secrets never runs; we only need to provide a 200 on
/// `/secrets/<name>/json` for each external secret so the engine's
/// `ensure_external_exists` preflight accepts it.
fn fake_routing() -> fake_podman::FakePodman {
	fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "POST" && target.contains("/containers/create") {
			(200, r#"{"Id":"abc","Warnings":[]}"#.to_string())
		} else if method == "POST" && target.contains("/start") {
			(200, String::new())
		} else if method == "DELETE" && target.contains("/containers/") {
			(404, r#"{"message":"no such container"}"#.to_string())
		} else if method == "GET" && target.contains("/secrets/") && target.ends_with("/json") {
			// External secrets exist on the host; respond 200 with a plausible body.
			(200, r#"{"ID":"ext","Spec":{"Labels":{}}}"#.to_string())
		} else {
			(404, r#"{"message":"unexpected request"}"#.to_string())
		}
	})
}

fn engine_for(fake: &fake_podman::FakePodman) -> Engine {
	Engine::with_base_dir(fake.client(), "proj".to_string(), std::env::temp_dir())
}

/// Pull the JSON body of the POST the engine sent to `/containers/create`.
/// `bodies` is parallel to `requests` in the order the requests arrived, so
/// the first non-empty body is the create call (every other call in this
/// scenario, list, pull, delete, start, and secret inspect, carries no body).
fn decode_create_body(bodies: &[Vec<u8>]) -> serde_json::Value {
	let body = bodies
		.iter()
		.find(|b| !b.is_empty())
		.expect("at least one request must carry a body (the create call)");
	serde_json::from_slice(body).expect("create body must be valid JSON")
}

#[tokio::test]
#[cfg(unix)]
async fn create_sends_every_compose_field_in_the_spec_generator() {
	let fake = fake_routing();
	let engine = engine_for(&fake);

	// Every compose key that maps onto the `SpecGenerator` literal in
	// `Engine::create_and_start`, set on one service. The parser applies the
	// same defaults (ProjectName, …) the real CLI path does, so a missing key
	// here is genuinely a missing key in the spec the engine would build.
	//
	// Annotations ride alongside labels on the spec but on a different JSON
	// key. `userns_mode`, `pid`, `ipc`, `cgroup`, `uts` all flow into
	// dedicated namespace slots; `cgroup_parent` is its own field.
	// `devices:` with `:rwm` puts a LinuxDevice on `devices` and the access
	// string onto `device_cgroup_rule` (the OCI `LinuxDevice` has no access
	// field). `device_cgroup_rules:` adds an extra structured rule. `cpus:
	// "0.5"` is the high-level cap; `cpu_shares` / `cpuset` /
	// `cpu_rt_runtime` / `cpu_rt_period` cover the low-level knobs that
	// compose still honours. `mem_limit` becomes `resource_limits.memory.limit`.
	// Every compose key that maps onto a `SpecGenerator` field, kept beside
	// this test as a file so the assertions below stay readable.
	let compose = include_str!("spec_body_fixture.yaml");

	let file = crate::compose::parse_str(compose).expect("compose must parse");
	let service = file
		.services
		.get("web")
		.expect("the fixture must declare a `web` service")
		.clone();

	engine
		.create_and_start("proj-web-1", "web", &service, &file, true)
		.await
		.expect("create_and_start must succeed against the fake socket");

	let bodies = fake.bodies.lock().unwrap().clone();
	assert!(
		bodies.iter().any(|b| !b.is_empty()),
		"the engine must have POSTed a body to /containers/create, got: {:?}",
		bodies
	);
	let body = decode_create_body(&bodies);

	// -- direct SpecGenerator scalars / strings --
	assert_eq!(body["name"], "proj-web-1");
	assert_eq!(body["image"], "example/web:1.2.3");
	assert_eq!(
		body["command"],
		serde_json::json!(["nginx", "-g", "daemon off;"])
	);
	assert_eq!(
		body["entrypoint"],
		serde_json::json!(["/docker-entrypoint.sh"])
	);
	assert_eq!(
		body["env"],
		serde_json::json!({"FOO": "bar", "EMPTY": "", "BAZ": "qux"})
	);
	assert_eq!(body["user"], "1000:1000");
	assert_eq!(body["work_dir"], "/app");
	assert_eq!(body["stop_signal"], 15);
	assert_eq!(body["stop_timeout"], 10);
	assert_eq!(body["hostname"], "my-host");
	assert_eq!(body["domainname"], "example.com");
	assert_eq!(
		body["cap_add"],
		serde_json::json!(["NET_ADMIN", "SYS_TIME"])
	);
	assert_eq!(body["cap_drop"], serde_json::json!(["MKNOD"]));
	assert_eq!(body["privileged"], false);
	assert_eq!(body["read_only_filesystem"], true);
	assert_eq!(body["cgroup_parent"], "/system.slice/podup");
	assert_eq!(body["shm_size"], serde_json::json!(64_i64 * 1024 * 1024));
	assert_eq!(body["init"], true);
	assert_eq!(body["restart_policy"], "on-failure");
	assert_eq!(body["restart_tries"], 5);
	assert_eq!(body["runtime"], "crun");
	assert_eq!(body["image_os"], "linux");
	assert_eq!(body["image_arch"], "amd64");
	assert_eq!(body["oom_score_adj"], -500);
	// (shm_size asserted in the spec block above with an i64 cast on the
	// literal so a 32-bit compile does not trip on `64 * 1024 * 1024`.)

	// -- annotations (HashMap<String, String>) --
	assert_eq!(
		body["annotations"],
		serde_json::json!({"org.opencontainers.image.title": "web"})
	);

	// -- labels: user labels plus the podup-injected trio. The config hash
	// is content-derived so we do not pin the exact hex; we assert the
	// three expected keys are present and the user label is forwarded. --
	let labels = body["labels"]
		.as_object()
		.expect("labels must be an object");
	assert_eq!(
		labels.get("svc.label").and_then(|v| v.as_str()),
		Some("present")
	);
	assert_eq!(
		labels.get("podup.project").and_then(|v| v.as_str()),
		Some("proj")
	);
	assert_eq!(
		labels.get("podup.service").and_then(|v| v.as_str()),
		Some("web")
	);
	let hash = labels
		.get("podup.config-hash")
		.and_then(|v| v.as_str())
		.expect("podup.config-hash must be set on the create spec");
	assert!(
		!hash.is_empty(),
		"config-hash must be a non-empty hex string"
	);
	// `podup.config-files` is stamped only when the engine knows about the
	// compose file paths, which the test does not feed in.
	assert!(
		!labels.contains_key("podup.config-files"),
		"the test engine has no compose-file paths, so this label must be absent"
	);

	// -- security_opt: decomposed onto six dedicated SpecGenerator fields. --
	assert_eq!(
		body["selinux_opts"],
		serde_json::json!(["user:role:svirt_lxc_net_t:s0"])
	);
	assert_eq!(body["apparmor_profile"], "runtime/default");
	assert_eq!(body["seccomp_profile_path"], "unconfined");
	assert_eq!(body["no_new_privileges"], true);
	// `mask:` is colon-split (see parse_security_opts): a single entry
	// `mask:/proc/asound` becomes one element, not five.
	assert_eq!(body["mask"], serde_json::json!(["/proc/asound"]));
	assert_eq!(body["unmask"], serde_json::json!(["/proc/asound"]));

	// -- sysctl (HashMap from compose sysctls:) --
	assert_eq!(
		body["sysctl"],
		serde_json::json!({"net.core.somaxconn": "1024"})
	);

	// -- port mappings: the parsed PortMapping struct (LibpodPortMapping
	// here), with `host_ip` preserved and `host_port` numeric. The
	// `host_ip` field is `skip_serializing_if = "String::is_empty"`, so
	// mappings without an explicit IP (the 9090/9091 ones) omit the key
	// entirely. --
	assert_eq!(
		body["portmappings"],
		serde_json::json!([
			{
				"container_port": 80,
				"host_port": 8080,
				"host_ip": "127.0.0.1",
				"protocol": "tcp",
			},
			{
				"container_port": 9090,
				"host_port": 9090,
				"protocol": "tcp",
			},
			{
				"container_port": 9091,
				"host_port": 9091,
				"protocol": "tcp",
			}
		])
	);

	// -- expose: {port_num => protocol} derived from the ports: list
	// (every published container port also shows up here, since
	// podup's expose map is the union of ports and explicit `expose:`)
	// plus any explicit `expose:` entries. --
	assert_eq!(
		body["expose"],
		serde_json::json!({"80": "tcp", "9000": "tcp", "9090": "tcp", "9091": "tcp"})
	);

	// -- networks: per-network options. The service is auto-registered as
	// an alias so `aliases` contains the service name plus the
	// user-supplied aliases. The compose-spec DNS contract puts the
	// service name at index 0 (so siblings resolve by name first);
	// user-supplied aliases follow. The network key on the wire is the
	// resolved name (`app-net`, the `name:` declared on the top-level
	// networks entry), not the compose-side key (`app`). libpod requires
	// netns=bridge whenever explicit networks are attached. --
	assert_eq!(
		body["networks"]["app-net"]["aliases"],
		serde_json::json!(["web", "web-alias"])
	);
	assert_eq!(body["netns"], serde_json::json!({"nsmode": "bridge"}));

	// -- extra_hosts is renamed to `hostadd` on the wire. --
	assert_eq!(
		body["hostadd"],
		serde_json::json!(["host1:1.2.3.4", "host2:5.6.7.8"])
	);

	// -- DNS triples --
	assert_eq!(body["dns_server"], serde_json::json!(["1.1.1.1"]));
	assert_eq!(body["dns_search"], serde_json::json!(["example.lan"]));
	assert_eq!(body["dns_option"], serde_json::json!(["ndots:2"]));

	// -- mounts: the bind mount (with `ro`) and the named volume ride in
	// different arrays on the spec; the bind stays in `mounts`, the named
	// volume goes to `volumes` (renamed `Name`/`Dest`/`Options`). --
	let mounts = body["mounts"].as_array().expect("mounts must be an array");
	// One bind (type=bind) and one tmpfs from the `tmpfs:` block.
	let tmpfs = mounts
		.iter()
		.find(|m| m["type"] == "tmpfs")
		.expect("tmpfs entry must appear under mounts");
	assert_eq!(tmpfs["destination"], "/tmp");
	// tmpfs options were split on the first colon: everything after the
	// destination's colon is options. `"size=64m,uid=1000"` was passed
	// through verbatim by parse_tmpfs_string.
	let opts = tmpfs["options"]
		.as_array()
		.expect("options must be an array");
	assert!(opts.iter().any(|o| o == "size=64m"));
	assert!(opts.iter().any(|o| o == "uid=1000"));
	assert!(tmpfs["source"].is_null(), "tmpfs source must be absent");

	let bind = mounts
		.iter()
		.find(|m| m["type"] == "bind")
		.expect("bind mount must appear under mounts");
	assert_eq!(bind["destination"], "/data");
	assert_eq!(bind["options"], serde_json::json!(["ro"]));
	// Bind sources are resolved against the engine's base_dir; the engine's
	// base_dir in this test is the process temp dir, so the source ends up
	// rooted there. Just assert the trailing segment survives.
	let bind_src = bind["source"]
		.as_str()
		.expect("bind source must be a string");
	assert!(
		bind_src.ends_with("host-data"),
		"bind source should resolve under the engine base_dir; got {bind_src:?}"
	);

	// -- volumes: the named-volume entry, with the project-prefixed name
	// (`proj_myvolume`; `myvolume` declared under top-level volumes:, no
	// custom `name:`, so the engine applies the project prefix). --
	let volumes = body["volumes"]
		.as_array()
		.expect("volumes must be an array");
	let named = volumes
		.iter()
		.find(|v| v["Dest"] == "/named")
		.expect("named volume must appear under volumes");
	assert_eq!(named["Name"], "proj_myvolume");
	assert_eq!(named["Options"], serde_json::json!(["ro"]));

	// -- volumes_from: resolved to the referenced service's container name
	// (no explicit container_name on `other-svc`, so the engine uses
	// `{project}-{other-svc}-1`), with the `:ro` mode preserved. --
	assert_eq!(
		body["volumes_from"],
		serde_json::json!(["proj-other-svc-1:ro"])
	);

	// -- secrets: external: true → the engine keeps the user-supplied name
	// (no project scoping), with the explicit long-form target. Even an
	// external source gets the compose-spec default mode of 0o444 (292):
	// see `push_plan` in the secrets planner; an external source has no
	// payload, but `from_file` is false, so the same `mode.or(0o444)`
	// fallback applies. --
	assert_eq!(
		body["secrets"],
		serde_json::json!([
			{"Source": "api_token", "Target": "/run/secrets/api_token", "Mode": 0o444}
		])
	);

	// -- namespace slots --
	assert_eq!(body["userns"], serde_json::json!({"nsmode": "keep-id"}));
	assert_eq!(body["pidns"], serde_json::json!({"nsmode": "host"}));
	assert_eq!(body["ipcns"], serde_json::json!({"nsmode": "private"}));

	// -- resource_limits: memory.limit from `mem_limit`, cpu from `cpus` +
	// `cpu_shares` + `cpuset`, pids from `pids_limit`. The 100ms period
	// (100_000us) is the engine-injected default when a cpus-derived
	// quota is set but no explicit `cpu_period:` is. The CPU quota is
	// `cpus * 1e9 / 10_000` = `0.5 * 1e9 / 10_000` = 50000. --
	let rl = &body["resource_limits"];
	assert_eq!(
		rl["memory"]["limit"],
		serde_json::json!(256_i64 * 1024 * 1024)
	);
	// `memswap_limit:` was not set on the fixture; the typed `swap` field
	// is `Option<i64>` with `skip_serializing_if = "Option::is_none"`, so
	// a missing key on the wire corresponds to JSON `null`.
	assert_eq!(rl["memory"]["swap"], serde_json::Value::Null);
	assert_eq!(rl["cpu"]["shares"], 512);
	assert_eq!(rl["cpu"]["quota"], 50_000);
	assert_eq!(rl["cpu"]["period"], 100_000);
	assert_eq!(rl["cpu"]["cpus"], "0,1");
	assert_eq!(rl["pids"]["limit"], 100);

	// -- ulimits → `r_limits` on the wire (rename). --
	assert_eq!(
		body["r_limits"],
		serde_json::json!([
			{"type": "nofile", "soft": 1024, "hard": 2048},
			{"type": "nproc", "soft": 64, "hard": 64}
		])
	);

	// -- healthconfig + health_check_on_failure_action. libpod reads the
	// fields under their PascalCase names. The compose interval/timeout
	// defaults (`30s` each) do not apply because the fixture sets both
	// explicitly. --
	let hc = &body["healthconfig"];
	assert_eq!(
		hc["Test"],
		serde_json::json!(["CMD", "curl", "-f", "http://localhost/"])
	);
	assert_eq!(hc["Interval"], serde_json::json!(5_i64 * 1_000_000_000));
	assert_eq!(hc["Timeout"], serde_json::json!(3_i64 * 1_000_000_000));
	assert_eq!(hc["Retries"], 4);
	assert_eq!(hc["StartPeriod"], serde_json::json!(10_i64 * 1_000_000_000));
	// `x-podman-on-failure: kill` → HealthCheckOnFailureAction::Kill (2).
	assert_eq!(body["health_check_on_failure_action"], 2);

	// -- log_configuration: libpod ignores max-size inside options (the
	// field the test checks is the typed `size`), and `max-file` would be
	// dropped with a warning. --
	assert_eq!(body["log_configuration"]["driver"], "json-file");
	assert_eq!(
		body["log_configuration"]["size"],
		serde_json::json!(5_i64 * 1024 * 1024)
	);
	assert!(
		body["log_configuration"]["options"]
			.as_object()
			.map(|o| o.is_empty())
			.unwrap_or(true),
		"after `max-size` was moved into the typed `size` field, options must be empty: {:?}",
		body["log_configuration"]["options"]
	);

	// -- devices: the parsed device entry (host path becomes the container
	// path when no `:container` segment is given). The `:rwm` access
	// rides alongside as a cgroup rule. --
	let devices = body["devices"]
		.as_array()
		.expect("devices must be an array");
	assert_eq!(devices.len(), 1);
	assert_eq!(devices[0]["path"], "/dev/null");
	// device_type is the device-type character (`c`/`b`/`p`/`u`) from the
	// host stat; it is the single field that depends on the host. Just
	// assert it's present and non-empty.
	assert!(
		devices[0]["type"].as_str().is_some_and(|s| !s.is_empty()),
		"device type must be set"
	);

	// -- device_cgroup_rule: two entries: the access string carried over
	// from `devices:`, plus the explicit `device_cgroup_rules:` entry. --
	let dcr = body["device_cgroup_rule"]
		.as_array()
		.expect("device_cgroup_rule must be an array");
	assert_eq!(dcr.len(), 2, "got device_cgroup_rule: {dcr:?}");

	// -- group_add → groups --
	assert_eq!(body["groups"], serde_json::json!(["1001"]));

	// -- links: a sibling-service reference resolves to
	// `{container}:{alias}`. `other-svc` has no explicit container_name,
	// so the engine builds the auto-generated name. --
	assert_eq!(
		body["links"],
		serde_json::json!(["proj-other-svc-1:other-alias"])
	);

	// -- storage_opt → storage_opts --
	assert_eq!(body["storage_opts"], serde_json::json!({"size": "1G"}));
}
