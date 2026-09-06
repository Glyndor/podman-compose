use super::*;
use crate::compose::types::{ComposeFile, EnvVars, Labels, Service};
use indexmap::IndexMap;

fn svc(image: &str) -> Service {
	Service {
		image: Some(image.to_string()),
		..Default::default()
	}
}

// merge_service: scalar fields

#[test]
fn merge_override_image_wins() {
	let base = svc("nginx:1.24");
	let over = svc("nginx:1.25");
	let merged = merge_service(base, over);
	assert_eq!(merged.image.as_deref(), Some("nginx:1.25"));
}

#[test]
fn merge_base_image_used_when_override_missing() {
	let base = svc("nginx:1.24");
	let over = Service::default();
	let merged = merge_service(base, over);
	assert_eq!(merged.image.as_deref(), Some("nginx:1.24"));
}

// merge_service: env vars (override wins per key)

#[test]
fn merge_env_vars_override_wins_on_conflict() {
	let mut base_map = IndexMap::new();
	base_map.insert(
		"PORT".to_string(),
		Some(serde_yaml::Value::String("8080".into())),
	);
	let base = Service {
		environment: EnvVars::Map(base_map),
		..Default::default()
	};

	let mut over_map = IndexMap::new();
	over_map.insert(
		"PORT".to_string(),
		Some(serde_yaml::Value::String("9090".into())),
	);
	let over = Service {
		environment: EnvVars::Map(over_map),
		..Default::default()
	};

	let merged = merge_service(base, over);
	let env = merged.environment.to_map();
	assert_eq!(env.get("PORT").and_then(|v| v.as_deref()), Some("9090"));
}

#[test]
fn merge_env_vars_base_key_preserved_when_not_overridden() {
	let mut base_map = IndexMap::new();
	base_map.insert(
		"BASE_ONLY".to_string(),
		Some(serde_yaml::Value::String("yes".into())),
	);
	let base = Service {
		environment: EnvVars::Map(base_map),
		..Default::default()
	};
	let over = Service::default();
	let merged = merge_service(base, over);
	let env = merged.environment.to_map();
	assert_eq!(env.get("BASE_ONLY").and_then(|v| v.as_deref()), Some("yes"));
}

// merge_service: labels (merged, override wins on conflict)

#[test]
fn merge_labels_both_preserved() {
	let mut base_im = IndexMap::new();
	base_im.insert("team".to_string(), "infra".to_string());
	let base = Service {
		labels: Labels::Map(base_im),
		..Default::default()
	};

	let mut over_im = IndexMap::new();
	over_im.insert("env".to_string(), "prod".to_string());
	let over = Service {
		labels: Labels::Map(over_im),
		..Default::default()
	};

	let merged = merge_service(base, over);
	let lm = merged.labels.to_map();
	assert_eq!(lm.get("team").map(|s| s.as_str()), Some("infra"));
	assert_eq!(lm.get("env").map(|s| s.as_str()), Some("prod"));
}

// resolve_extends_same_file: cycle detection

#[test]
fn cycle_detection_returns_error() {
	use crate::compose::types::ExtendsConfig;
	let mut file = ComposeFile::default();
	file.services.insert(
		"a".to_string(),
		Service {
			extends: Some(ExtendsConfig::Service("b".to_string())),
			..Default::default()
		},
	);
	file.services.insert(
		"b".to_string(),
		Service {
			extends: Some(ExtendsConfig::Service("a".to_string())),
			..Default::default()
		},
	);
	assert!(resolve_extends_same_file(&mut file).is_err());
}

#[test]
fn self_extends_returns_error() {
	use crate::compose::types::ExtendsConfig;
	let mut file = ComposeFile::default();
	file.services.insert(
		"web".to_string(),
		Service {
			extends: Some(ExtendsConfig::Service("web".to_string())),
			..Default::default()
		},
	);
	assert!(resolve_extends_same_file(&mut file).is_err());
}

#[test]
fn extends_unknown_service_returns_error() {
	use crate::compose::types::ExtendsConfig;
	let mut file = ComposeFile::default();
	file.services.insert(
		"web".to_string(),
		Service {
			extends: Some(ExtendsConfig::Service("nonexistent".to_string())),
			..Default::default()
		},
	);
	assert!(resolve_extends_same_file(&mut file).is_err());
}

#[test]
fn extends_inherits_image_from_base() {
	use crate::compose::types::ExtendsConfig;
	let mut file = ComposeFile::default();
	file.services.insert("base".to_string(), svc("postgres:16"));
	file.services.insert(
		"db".to_string(),
		Service {
			extends: Some(ExtendsConfig::Service("base".to_string())),
			..Default::default()
		},
	);
	resolve_extends_same_file(&mut file).unwrap();
	assert_eq!(file.services["db"].image.as_deref(), Some("postgres:16"));
	assert!(file.services["db"].extends.is_none());
}
