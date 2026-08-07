use super::unit_named;
use crate::parse_str;
use crate::quadlet::generate_at;

#[test]
fn maps_the_full_container_field_set() {
	let yaml = r#"
services:
  app:
    image: app:1.0
    hostname: app-host
    user: "1000:1000"
    working_dir: /srv
    read_only: true
    init: true
    entrypoint: ["/bin/sh", "-c"]
    command: server --port 9000
    labels:
      z_team: core
      a_tier: web
    cap_add:
      - NET_ADMIN
    cap_drop:
      - MKNOD
    ports:
      - target: 9000
        published: 9000
        protocol: udp
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "proj-app.container").contents;
	assert!(c.contains("HostName=app-host"));
	// `user: "1000:1000"` splits into separate User=/Group= keys.
	assert!(c.contains("User=1000"));
	assert!(c.contains("Group=1000"));
	assert!(!c.contains("User=1000:1000"));
	assert!(c.contains("WorkingDir=/srv"));
	assert!(c.contains("ReadOnly=true"));
	assert!(c.contains("RunInit=true"));
	assert!(c.contains("Entrypoint=/bin/sh -c"));
	assert!(c.contains("Exec=server --port 9000"));
	assert!(c.contains("AddCapability=NET_ADMIN"));
	assert!(c.contains("DropCapability=MKNOD"));
	assert!(c.contains("PublishPort=9000:9000/udp"));
	// Labels sorted by key.
	let a = c.find("Label=a_tier=web").unwrap();
	let z = c.find("Label=z_team=core").unwrap();
	assert!(a < z, "labels must be sorted");
}

#[test]
fn restart_policies_map_to_systemd() {
	let cases = [
		("no", "Restart=no"),
		("always", "Restart=always"),
		("unless-stopped", "Restart=always"),
		("on-failure", "Restart=on-failure"),
	];
	for (policy, expected) in cases {
		let yaml = format!("services:\n  s:\n    image: x\n    restart: {policy}\n");
		let file = parse_str(&yaml).unwrap();
		let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
		assert!(
			unit_named(&out, "p-s.container")
				.contents
				.contains(expected),
			"{policy} -> {expected}"
		);
	}
}

#[test]
fn optional_dependency_uses_wants_not_requires() {
	let yaml = r#"
services:
  web:
    image: nginx
    depends_on:
      cache:
        condition: service_started
        required: false
  cache:
    image: redis
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "proj-web.container").contents;
	assert!(c.contains("After=proj-cache.service"));
	assert!(c.contains("Wants=proj-cache.service"));
	assert!(!c.contains("Requires=proj-cache.service"));
}

#[test]
fn maps_extended_container_field_set() {
	let yaml = r#"
services:
  app:
    image: x
    container_name: custom
    env_file:
      - ./app.env
    tmpfs:
      - /run
    sysctls:
      net.core.somaxconn: "1024"
    ulimits:
      nofile:
        soft: 1024
        hard: 2048
    shm_size: 64m
    mem_limit: 512m
    pids_limit: 100
    userns_mode: keep-id
    stop_signal: SIGTERM
    stop_grace_period: 30s
    devices:
      - /dev/fuse
    dns:
      - 1.1.1.1
    extra_hosts:
      - "db:10.0.0.2"
    annotations:
      run.oci.keep: "1"
    network_mode: host
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost"]
      interval: 5s
      retries: 3
    restart: "on-failure:5"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-app.container").contents;
	// Absolute, resolved against the compose base dir: systemd resolves a relative
	// `EnvironmentFile=` against the unit's own directory, not the project's, so
	// `./app.env` would never be found once installed. Built with `join` so the
	// separator matches the host (this test is not Unix-gated).
	let env_file = format!(
		"EnvironmentFile={}",
		std::path::Path::new("/srv/app").join("app.env").display()
	);
	for needle in [
		"ContainerName=custom",
		&env_file,
		"Tmpfs=/run",
		"Sysctl=net.core.somaxconn=1024",
		"Ulimit=nofile=1024:2048",
		"ShmSize=64m",
		"PidsLimit=100",
		"UserNS=keep-id",
		"StopSignal=SIGTERM",
		"StopTimeout=30",
		"AddDevice=/dev/fuse",
		"DNS=1.1.1.1",
		"AddHost=db:10.0.0.2",
		"Annotation=run.oci.keep=1",
		"Network=host",
		"PodmanArgs=--memory=512m",
		"HealthCmd=curl -f http://localhost",
		"HealthInterval=5s",
		"HealthRetries=3",
		"StartLimitBurst=5",
	] {
		assert!(c.contains(needle), "missing `{needle}` in:\n{c}");
	}
}

#[test]
fn ephemeral_published_port_omits_host_side() {
	let yaml = r#"
services:
  s:
    image: x
    ports:
      - "80"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("PublishPort=80"));
	assert!(!c.contains("PublishPort=:80"));
}

#[test]
fn user_with_gid_splits_into_user_and_group() {
	let yaml = "services:\n  s:\n    image: x\n    user: \"1000:2000\"\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("User=1000"));
	assert!(c.contains("Group=2000"));
	assert!(!c.contains("User=1000:2000"));
}

#[test]
fn service_secret_maps_to_secret_key() {
	let yaml = r#"
services:
  s:
    image: x
    secrets:
      - tok
      - source: cred
        target: /run/cred
        uid: "100"
secrets:
  tok:
    file: ./tok
  cred:
    file: ./cred
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("Secret=tok"));
	assert!(c.contains("Secret=cred,target=/run/cred,uid=100"));
	assert!(!out.warnings.iter().any(|w| w.contains("secrets")));
}

#[test]
fn maps_previously_dropped_container_fields() {
	let yaml = r#"
services:
  s:
    image: x
    group_add:
      - audio
    expose:
      - "8080"
    security_opt:
      - "no-new-privileges:true"
      - "seccomp=/etc/seccomp.json"
      - "label=type:container_t"
    pull_policy: always
    logging:
      driver: journald
      options:
        tag: mytag
    networks:
      net:
        aliases:
          - web-alias
    deploy:
      resources:
        limits:
          memory: 256m
networks:
  net:
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	for needle in [
		"GroupAdd=audio",
		"ExposeHostPort=8080",
		"NoNewPrivileges=true",
		"SeccompProfile=/etc/seccomp.json",
		"SecurityLabelType=container_t",
		"Pull=always",
		"LogDriver=journald",
		"LogOpt=tag=mytag",
		"NetworkAlias=web-alias",
		"PodmanArgs=--memory=256m",
	] {
		assert!(c.contains(needle), "missing `{needle}` in:\n{c}");
	}
}

#[test]
fn memory_and_apparmor_render_as_podman_args() {
	// `Memory=` and `AppArmor=` are not recognised [Container] keys in
	// podman-systemd.unit(5) (Quadlet drops the whole unit at daemon-reload), so
	// they must route through `PodmanArgs=` like the CPU limits, not be emitted as
	// native keys.
	let yaml = r#"
services:
  s:
    image: app:1.0
    mem_limit: 512m
    security_opt:
      - "apparmor=my-profile"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("PodmanArgs=--memory=512m"),
		"mem_limit must route through PodmanArgs in:\n{c}"
	);
	assert!(
		c.contains("PodmanArgs=--security-opt apparmor=my-profile"),
		"apparmor must route through PodmanArgs in:\n{c}"
	);
	for forbidden in ["Memory=512m", "AppArmor=my-profile"] {
		assert!(
			!c.contains(forbidden),
			"memory/apparmor must not use an unrecognised native key `{forbidden}` in:\n{c}"
		);
	}
}

#[test]
fn cpu_limits_render_as_podman_args() {
	// CPU limits have no native [Container] Quadlet key; they must round-trip
	// through PodmanArgs= rather than being silently dropped.
	let yaml = r#"
services:
  s:
    image: app:1.0
    cpus: "1.5"
    cpuset: "0,1"
    cpu_shares: 512
    cpu_quota: 50000
    cpu_period: 100000
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	for expected in [
		"PodmanArgs=--cpus=1.5",
		"PodmanArgs=--cpuset-cpus=0,1",
		"PodmanArgs=--cpu-shares=512",
		"PodmanArgs=--cpu-quota=50000",
		"PodmanArgs=--cpu-period=100000",
	] {
		assert!(c.contains(expected), "missing `{expected}` in:\n{c}");
	}
}

#[test]
fn deploy_limits_cpus_render_as_podman_args() {
	// `deploy.resources.limits.cpus` is the modern equivalent of `cpus` and
	// must reach the unit too when the top-level `cpus` is absent.
	let yaml = r#"
services:
  s:
    image: app:1.0
    deploy:
      resources:
        limits:
          cpus: "2"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("PodmanArgs=--cpus=2"),
		"missing deploy cpus PodmanArgs in:\n{c}"
	);
}

#[test]
fn static_ips_render_as_ip_keys() {
	// `networks.<n>.ipv4_address`/`ipv6_address` must reach the unit as IP=/IP6=,
	// not be silently dropped.
	let yaml = r#"
services:
  s:
    image: x
    networks:
      net:
        ipv4_address: 10.5.0.7
        ipv6_address: "2001:db8::7"
networks:
  net:
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("IP=10.5.0.7"), "missing IP= in:\n{c}");
	assert!(c.contains("IP6=2001:db8::7"), "missing IP6= in:\n{c}");
}

#[test]
fn network_mode_none_maps_to_network_none() {
	// `network_mode: none` is a valid Quadlet value; it must map to Network=none
	// rather than being warned and dropped.
	let yaml = "services:\n  s:\n    image: x\n    network_mode: none\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("Network=none"), "missing Network=none in:\n{c}");
	assert!(
		!out.warnings.iter().any(|w| w.contains("network_mode")),
		"network_mode: none must not warn; got: {:?}",
		out.warnings
	);
}

#[test]
fn security_opt_filetype_and_nested_map_to_keys() {
	let yaml = r#"
services:
  s:
    image: x
    security_opt:
      - "label=filetype:usr_t"
      - "label=nested"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("SecurityLabelFileType=usr_t"), "in:\n{c}");
	assert!(c.contains("SecurityLabelNested=true"), "in:\n{c}");
	assert!(
		!out.warnings.iter().any(|w| w.contains("security_opt")),
		"mapped labels must not warn; got: {:?}",
		out.warnings
	);
}

#[test]
fn deploy_restart_policy_maps_to_systemd() {
	let yaml = r#"
services:
  s:
    image: x
    deploy:
      restart_policy:
        condition: on-failure
        max_attempts: 4
        window: 2m
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("Restart=on-failure"), "in:\n{c}");
	assert!(c.contains("StartLimitBurst=4"), "in:\n{c}");
	assert!(c.contains("StartLimitIntervalSec=120"), "in:\n{c}");
}

#[test]
fn deploy_restart_condition_none_maps_to_no() {
	let yaml = "services:\n  s:\n    image: x\n    deploy:\n      restart_policy:\n        condition: none\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	assert!(unit_named(&out, "p-s.container")
		.contents
		.contains("Restart=no"));
}

#[test]
fn deploy_limits_pids_maps_to_pids_limit() {
	let yaml = r#"
services:
  s:
    image: x
    deploy:
      resources:
        limits:
          pids: 256
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("PidsLimit=256"), "missing PidsLimit in:\n{c}");
}

#[test]
fn long_form_tmpfs_mount_renders_as_tmpfs_not_volume() {
	// A long-form `type: tmpfs` mount must become `Tmpfs=`, not `Volume=` —
	// the latter would persist it as a volume instead of an in-memory fs.
	let yaml = r#"
services:
  s:
    image: app:1.0
    volumes:
      - type: tmpfs
        target: /cache
        tmpfs:
          size: 64000000
          mode: 0o755
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("Tmpfs=/cache:size=64000000,mode=755"),
		"tmpfs not rendered as Tmpfs= with options in:\n{c}"
	);
	assert!(
		!c.contains("Volume=/cache"),
		"tmpfs wrongly emitted as a Volume in:\n{c}"
	);
}

/// #1091: `EnvironmentFile=` is resolved by podman-systemd.unit(5) against the
/// unit file's own directory, not the compose file's. Units land in
/// `~/.config/containers/systemd`, so a relative entry emitted verbatim points
/// at a file that is not there — and `--env-file` on a missing path is fatal, so
/// the container never starts. Every relative entry must come out absolute
/// against the compose base directory; an already-absolute one is untouched.
#[test]
fn env_file_entries_are_absolute_against_the_compose_base_dir() {
	let yaml = r#"
services:
  app:
    image: app:1.0
    env_file:
      - .env
      - ./config/extra.env
      - ../shared/team.env
      - /etc/glyndor/absolute.env
"#;
	let file = parse_str(yaml).unwrap();
	// A base directory that is deliberately not the process's cwd, so a bug that
	// resolved against the cwd instead would still show up here.
	let base = std::path::Path::new("/srv/app");
	let out = generate_at(&file, "p", base);
	let c = &unit_named(&out, "p-app.container").contents;
	// `abs_against` joins with the OS separator, so build the expectations the
	// same way rather than as POSIX literals — these render tests are not
	// Unix-gated and would otherwise fail on Windows.
	for rel in [".env", "config/extra.env", "../shared/team.env"] {
		let needle = format!("EnvironmentFile={}", base.join(rel).display());
		assert!(c.contains(&needle), "missing `{needle}` in:\n{c}");
	}
	// An already-absolute entry is passed through verbatim, separators included.
	assert!(
		c.contains("EnvironmentFile=/etc/glyndor/absolute.env"),
		"an absolute entry must be untouched in:\n{c}"
	);
	// No entry may survive as a compose-relative path: systemd would resolve it
	// against the unit directory. Checked by prefix rather than `is_absolute()`,
	// which on Windows demands a drive letter that a `/srv/app` base never has.
	let base_prefix = base.display().to_string();
	for line in c.lines().filter(|l| l.starts_with("EnvironmentFile=")) {
		let value = line.trim_start_matches("EnvironmentFile=");
		assert!(
			value.starts_with(&base_prefix) || value.starts_with("/etc/"),
			"`{line}` was not resolved against the compose base directory"
		);
	}
}
