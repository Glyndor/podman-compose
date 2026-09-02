use super::*;

#[test]
fn exec_create_config_serializes_cmd() {
	let cfg = ExecCreateConfig {
		cmd: Some(vec!["sh".into(), "-c".into(), "echo hi".into()]),
		attach_stdout: Some(true),
		attach_stderr: Some(true),
		..Default::default()
	};
	let v = serde_json::to_value(&cfg).unwrap();
	assert_eq!(v["Cmd"], serde_json::json!(["sh", "-c", "echo hi"]));
	assert_eq!(v["AttachStdout"], serde_json::json!(true));
}

#[test]
fn exec_create_config_skips_none_fields() {
	let cfg = ExecCreateConfig::default();
	let v = serde_json::to_value(&cfg).unwrap();
	assert!(v.get("Cmd").is_none());
	assert!(v.get("User").is_none());
}

#[test]
fn exec_start_config_serializes() {
	let cfg = ExecStartConfig {
		detach: false,
		tty: true,
	};
	let v = serde_json::to_value(&cfg).unwrap();
	assert_eq!(v["Detach"], serde_json::json!(false));
	assert_eq!(v["Tty"], serde_json::json!(true));
}

#[test]
fn exec_create_response_deserialize() {
	let json = r#"{"Id": "abc123"}"#;
	let r: ExecCreateResponse = serde_json::from_str(json).unwrap();
	assert_eq!(r.id, "abc123");
}

#[test]
fn exec_inspect_deserialize() {
	let json = r#"{"ExitCode": 1}"#;
	let r: ExecInspect = serde_json::from_str(json).unwrap();
	assert_eq!(r.exit_code, Some(1));
}
