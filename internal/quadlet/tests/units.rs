use super::unit_named;
use crate::parse_str;
use crate::quadlet::{generate_at, QuadletOutput, QuadletUnit};

#[test]
fn container_name_defaults_to_project_prefixed() {
	// A service with no explicit `container_name:` must default to
	// `{project}-{service}`, matching how `up` names the running container,
	// rather than a bare `web` that would collide across projects.
	let file = parse_str("services:\n  web:\n    image: nginx\n").unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let web = unit_named(&out, "proj-web.container");
	assert!(web.contents.contains("ContainerName=proj-web"));
}

#[test]
fn duplicate_filename_detects_collision() {
	let mk = |n: &str| QuadletUnit {
		filename: n.to_string(),
		contents: String::new(),
	};
	let mut out = QuadletOutput {
		units: vec![mk("web.container"), mk("db.volume")],
		..Default::default()
	};
	assert_eq!(out.duplicate_filename(), None);
	out.units.push(mk("web.container"));
	assert_eq!(out.duplicate_filename(), Some("web.container"));
}

#[test]
fn generates_container_network_and_volume_units() {
	let yaml = r#"
services:
  web:
    image: nginx:1.27
    container_name: web
    ports:
      - "8080:80"
    environment:
      B_KEY: two
      A_KEY: one
    volumes:
      - data:/var/lib/data
    networks:
      - frontend
    restart: unless-stopped
    depends_on:
      - db
  db:
    image: postgres:16
volumes:
  data:
networks:
  frontend:
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));

	let web = unit_named(&out, "proj-web.container");
	assert!(web.contents.contains("Image=nginx:1.27"));
	assert!(web.contents.contains("ContainerName=web"));
	assert!(web.contents.contains("PublishPort=8080:80"));
	// Environment is emitted in sorted key order for determinism.
	let a = web.contents.find("Environment=A_KEY=one").unwrap();
	let b = web.contents.find("Environment=B_KEY=two").unwrap();
	assert!(a < b, "environment keys must be sorted");
	// Declared named volume is tied to its .volume unit.
	assert!(web
		.contents
		.contains("Volume=proj-data.volume:/var/lib/data"));
	assert!(web.contents.contains("Network=proj-frontend.network"));
	// unless-stopped maps to systemd Restart=always.
	assert!(web.contents.contains("Restart=always"));
	assert!(web.contents.contains("After=proj-db.service"));
	assert!(web.contents.contains("WantedBy=default.target"));

	unit_named(&out, "proj-db.container");
	assert!(unit_named(&out, "proj-data.volume")
		.contents
		.contains("VolumeName=proj_data"));
	assert!(unit_named(&out, "proj-frontend.network")
		.contents
		.contains("NetworkName=proj_frontend"));
}

#[test]
fn build_field_emits_a_build_unit() {
	let yaml = r#"
services:
  app:
    build:
      context: ./src
      dockerfile: Dockerfile.app
      target: runtime
    image: app:latest
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	// A `.build` unit is generated (no longer just a warning) and the container
	// references it so Quadlet builds before running.
	let build = out
		.units
		.iter()
		.find(|u| u.filename == "proj-app.build")
		.expect("a build service must emit an app.build unit");
	assert!(build.contents.contains("[Build]"));
	assert!(build.contents.contains("ImageTag=app:latest"));
	// `abs_context` joins with the OS separator, so build the expected path the
	// same way (`/srv/app/src` on Unix, `/srv/app\src` on Windows) rather than a
	// POSIX literal that fails the render tests (which are not Unix-gated) on Windows.
	let expected_ctx = format!(
		"SetWorkingDirectory={}",
		std::path::Path::new("/srv/app").join("src").display()
	);
	assert!(build.contents.contains(&expected_ctx), "{}", build.contents);
	assert!(build.contents.contains("File=Dockerfile.app"));
	assert!(build.contents.contains("Target=runtime"));
	let container = out
		.units
		.iter()
		.find(|u| u.filename == "proj-app.container")
		.unwrap();
	assert!(container.contents.contains("Image=proj-app.build"));
	assert!(!out.warnings.iter().any(|w| w.contains("build")));
}

#[test]
fn inline_dockerfile_build_warns_and_emits_no_build_unit() {
	let yaml = "services:\n  app:\n    build:\n      dockerfile_inline: \"FROM alpine\"\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	assert!(!out.units.iter().any(|u| u.filename == "proj-app.build"));
	assert!(out.warnings.iter().any(|w| w.contains("dockerfile_inline")));
}

#[test]
fn bind_path_volume_is_passed_through() {
	let yaml = r#"
services:
  web:
    image: nginx
    volumes:
      - ./html:/usr/share/nginx/html:ro
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let web = unit_named(&out, "proj-web.container");
	assert!(web
		.contents
		.contains("Volume=./html:/usr/share/nginx/html:ro"));
}

#[test]
fn long_form_volume_with_named_source_and_readonly() {
	let yaml = r#"
services:
  db:
    image: postgres
    volumes:
      - type: volume
        source: pgdata
        target: /var/lib/postgresql/data
        read_only: true
volumes:
  pgdata:
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "proj-db.container").contents;
	assert!(c.contains("Volume=proj-pgdata.volume:/var/lib/postgresql/data:ro"));
}

#[test]
fn warns_for_every_unmapped_field() {
	let yaml = r#"
services:
  s:
    image: x
    network_mode: "bridge:custom"
    profiles: [debug]
    volumes_from:
      - other
    deploy:
      replicas: 3
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let joined = out.warnings.join("\n");
	for needle in ["network_mode", "profiles", "volumes_from", "scale/replicas"] {
		assert!(joined.contains(needle), "expected warning for {needle}");
	}
}

/// #1092: `env_file: [{path, required: false}]` says a missing file is fine.
/// Quadlet cannot express that: `EnvironmentFile=` becomes `--env-file`, which
/// is fatal on a missing path, so the entry is emitted anyway and the container
/// refuses to start. That is the only behaviour available; what must not happen
/// is emitting it silently.
#[test]
fn warns_when_an_optional_env_file_cannot_stay_optional() {
	let yaml = r#"
services:
  s:
    image: x
    env_file:
      - path: .env
        required: true
      - path: .env.production
        required: false
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let joined = out.warnings.join("\n");
	assert!(
		joined.contains("env_file") && joined.contains("required: false"),
		"expected a warning naming the unmappable `required: false`, got:\n{joined}"
	);
	// The entry is still emitted; dropping it would lose configuration the user
	// asked for whenever the file does exist. Built with `join` so the separator
	// matches the host (this test is not Unix-gated).
	let c = &unit_named(&out, "p-s.container").contents;
	let needle = format!(
		"EnvironmentFile={}",
		std::path::Path::new("/srv/app")
			.join(".env.production")
			.display()
	);
	assert!(c.contains(&needle), "missing `{needle}` in:\n{c}");
}

/// The warning is about `required: false` specifically, so an env_file list
/// where every entry is required must stay quiet.
#[test]
fn no_env_file_warning_when_every_entry_is_required() {
	let yaml = "services:\n  s:\n    image: x\n    env_file:\n      - .env\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	assert!(
		!out.warnings.iter().any(|w| w.contains("env_file")),
		"unexpected env_file warning: {:?}",
		out.warnings
	);
}

#[test]
fn hostile_service_name_cannot_escape_output_directory() {
	// A compose key containing path separators must never yield a unit
	// file name that escapes the output directory.
	let yaml = "services:\n  ? \"../../evil\"\n  : { image: x }\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let unit = &out.units[0];
	assert!(
		!unit.filename.contains('/') && !unit.filename.contains('\\'),
		"unit file name must be a single safe component, got {}",
		unit.filename
	);
	assert!(unit.filename.ends_with(".container"));
}

#[test]
fn newline_in_value_cannot_inject_unit_directives() {
	// An environment value carrying a newline plus a forged directive must
	// be flattened to a single line, not injected as a new unit entry.
	let yaml =
		"services:\n  web:\n    image: x\n    environment:\n      EVIL: \"a\\nExecStartPre=/bin/rm -rf /\"\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "proj-web.container").contents;
	assert!(
		!c.lines().any(|l| l.starts_with("ExecStartPre")),
		"a newline in a value must not inject a directive line:\n{c}"
	);
}

#[test]
fn privileged_maps_to_podman_arg() {
	let yaml = "services:\n  s:\n    image: x\n    privileged: true\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("PodmanArgs=--privileged"), "in:\n{c}");
	assert!(
		!out.warnings.iter().any(|w| w.contains("privileged")),
		"privileged must be mapped, not warned; got: {:?}",
		out.warnings
	);
}

#[test]
fn units_carry_podup_ownership_labels() {
	// Every generated unit must carry the same ownership labels the live engine
	// stamps: `podup.project` on all three unit types and `podup.service` on the
	// container, so exported resources are traceable back to their project.
	let yaml = r#"
services:
  web:
    image: nginx:1.27
networks:
  net:
volumes:
  vol:
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));

	let c = &unit_named(&out, "proj-web.container").contents;
	assert!(
		c.contains("Label=podup.project=proj"),
		"container missing project ownership label in:\n{c}"
	);
	assert!(
		c.contains("Label=podup.service=web"),
		"container missing service ownership label in:\n{c}"
	);

	let net = &unit_named(&out, "proj-net.network").contents;
	assert!(
		net.contains("Label=podup.project=proj"),
		"network missing project ownership label in:\n{net}"
	);
	// Networks/volumes are project-scoped, not service-scoped: no service label.
	assert!(
		!net.contains("podup.service"),
		"network must not carry a service label in:\n{net}"
	);

	let vol = &unit_named(&out, "proj-vol.volume").contents;
	assert!(
		vol.contains("Label=podup.project=proj"),
		"volume missing project ownership label in:\n{vol}"
	);
	assert!(
		!vol.contains("podup.service"),
		"volume must not carry a service label in:\n{vol}"
	);
}
