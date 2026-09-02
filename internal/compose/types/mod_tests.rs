use crate::parse_str;

#[test]
fn redacts_inline_secret_and_config_content() {
	let yaml = r#"
secrets:
  inline_secret:
    content: super-secret-value
  file_secret:
    file: ./token.txt
configs:
  inline_config:
    content: embedded-config-body
  env_config:
    environment: CONFIG_FROM_ENV
"#;
	let mut file = parse_str(yaml).unwrap();
	file.redact_inline_content();

	assert_eq!(
		file.secrets["inline_secret"].content.as_deref(),
		Some("<redacted>")
	);
	// A `file:` source carries no embedded value, so nothing to redact.
	assert!(file.secrets["file_secret"].content.is_none());
	assert_eq!(
		file.secrets["file_secret"].file.as_deref(),
		Some("./token.txt")
	);

	assert_eq!(
		file.configs["inline_config"].content.as_deref(),
		Some("<redacted>")
	);
	// An `environment:` source names an env var; the value is not embedded.
	assert!(file.configs["env_config"].content.is_none());
	assert_eq!(
		file.configs["env_config"].environment.as_deref(),
		Some("CONFIG_FROM_ENV")
	);
}

#[test]
fn rendered_config_never_contains_inline_secret_value() {
	let yaml = r#"
secrets:
  db_password:
    content: hunter2-do-not-leak
"#;
	let mut file = parse_str(yaml).unwrap();
	file.redact_inline_content();
	let rendered = serde_yaml::to_string(&file).unwrap();
	assert!(!rendered.contains("hunter2-do-not-leak"));
	assert!(rendered.contains("<redacted>"));
}

#[test]
fn strip_ignored_unknown_keys_drops_non_extension_at_every_level() {
	// Unknown keys at the top level, in a service, a service sub-object
	// (deploy), a network, and a volume are all dropped, while `x-*` extension
	// keys at any level survive — so the rendered config agrees with the
	// diagnostics that flagged the rest as ignored.
	let yaml = r#"
bogus_top: 1
x-keep-top: ok
services:
  web:
    image: nginx
    bogus_svc: 2
    x-keep-svc: ok
    deploy:
      bogus_deploy: 3
networks:
  netA:
    bogus_net: 4
volumes:
  volA:
    bogus_vol: 5
"#;
	let mut file = parse_str(yaml).unwrap();
	file.strip_ignored_unknown_keys();
	let out = serde_yaml::to_string(&file).unwrap();
	for dropped in [
		"bogus_top",
		"bogus_svc",
		"bogus_deploy",
		"bogus_net",
		"bogus_vol",
	] {
		assert!(
			!out.contains(dropped),
			"{dropped} should be stripped: {out}"
		);
	}
	assert!(out.contains("x-keep-top"), "top x- kept: {out}");
	assert!(out.contains("x-keep-svc"), "svc x- kept: {out}");
}
