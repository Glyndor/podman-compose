use super::*;
use crate::compose::types::Service;

fn default_service() -> Service {
	Service::default()
}

// --- resource limits ---

#[test]
fn build_resource_limits_empty_service() {
	assert!(build_resource_limits(&default_service()).is_none());
}

#[test]
fn build_resource_limits_mem_limit() {
	let mut svc = default_service();
	svc.mem_limit = Some("512m".into());
	let res = build_resource_limits(&svc).unwrap();
	assert_eq!(res.memory.unwrap().limit, Some(512 * 1024 * 1024));
}

#[test]
fn build_resource_limits_deploy_overrides() {
	use crate::compose::types::{DeployConfig, ResourceSpec, ResourcesConfig};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		resources: Some(ResourcesConfig {
			limits: Some(ResourceSpec {
				memory: Some("256m".into()),
				..Default::default()
			}),
			reservations: None,
		}),
		..Default::default()
	});
	let res = build_resource_limits(&svc).unwrap();
	assert_eq!(res.memory.unwrap().limit, Some(256 * 1024 * 1024));
}

#[test]
fn build_resource_limits_deploy_cpus_pids_and_reservation() {
	// With no top-level cpus/pids/mem_reservation, the deploy block supplies
	// them: limits.cpus → quota, limits.pids → pids limit, reservations.memory
	// → memory soft limit.
	use crate::compose::types::{DeployConfig, ResourceSpec, ResourcesConfig};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		resources: Some(ResourcesConfig {
			limits: Some(ResourceSpec {
				cpus: Some("2".into()),
				pids: Some(512),
				..Default::default()
			}),
			reservations: Some(ResourceSpec {
				memory: Some("128m".into()),
				..Default::default()
			}),
		}),
		..Default::default()
	});
	let res = build_resource_limits(&svc).unwrap();
	// 2 CPUs → 2e9 nano_cpus → quota = 200_000.
	assert_eq!(res.cpu.unwrap().quota, Some(200_000));
	assert_eq!(res.pids.unwrap().limit, 512);
	assert_eq!(res.memory.unwrap().reservation, Some(128 * 1024 * 1024));
}

#[test]
fn build_resource_limits_cpus_converts_to_quota() {
	let mut svc = default_service();
	svc.cpus = Some("0.5".into());
	let res = build_resource_limits(&svc).unwrap();
	let cpu = res.cpu.unwrap();
	// 0.5 CPUs → 500_000_000 nano_cpus → quota = 50_000 (50ms per 100ms period)
	assert_eq!(cpu.quota, Some(50_000));
	assert_eq!(cpu.period, Some(100_000));
}

// --- ulimits ---

#[test]
fn build_ulimits_single_value() {
	use crate::compose::types::UlimitConfig;
	let mut svc = default_service();
	svc.ulimits
		.insert("nofile".to_string(), UlimitConfig::Single(1024));
	let ul = build_ulimits(&svc);
	assert_eq!(ul.len(), 1);
	assert_eq!(ul[0].ulimit_type, "nofile");
	assert_eq!(ul[0].soft, 1024);
	assert_eq!(ul[0].hard, 1024);
}

#[test]
fn build_ulimits_pair() {
	use crate::compose::types::UlimitConfig;
	let mut svc = default_service();
	svc.ulimits.insert(
		"nofile".to_string(),
		UlimitConfig::Pair {
			soft: 512,
			hard: 2048,
		},
	);
	let ul = build_ulimits(&svc);
	assert_eq!(ul[0].soft, 512);
	assert_eq!(ul[0].hard, 2048);
}

#[test]
fn build_ulimits_clamps_soft_above_hard() {
	use crate::compose::types::UlimitConfig;
	let mut svc = default_service();
	svc.ulimits.insert(
		"nofile".to_string(),
		UlimitConfig::Pair {
			soft: 65535,
			hard: 1024,
		},
	);
	let ul = build_ulimits(&svc);
	assert_eq!(ul[0].soft, 1024, "soft must be clamped down to hard");
	assert_eq!(ul[0].hard, 1024);
}

#[test]
fn build_ulimits_rejects_unknown_resource_name() {
	use crate::compose::types::UlimitConfig;
	let mut svc = default_service();
	svc.ulimits
		.insert("bogus,inject=1".to_string(), UlimitConfig::Single(1024));
	assert!(
		build_ulimits(&svc).is_empty(),
		"an unknown ulimit name must be dropped, not forwarded"
	);
}

// --- cdi devices ---

fn cdi_for(yaml: &str) -> Vec<String> {
	let file = crate::parse_str(yaml).unwrap();
	cdi_devices(&file.services["app"])
}

#[test]
fn cdi_gpu_count_all() {
	let got = cdi_for(
		"services:\n  app:\n    image: x\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - capabilities: [gpu]\n              count: all\n",
	);
	assert_eq!(got, vec!["nvidia.com/gpu=all"]);
}

#[test]
fn cdi_gpu_count_n_enumerates() {
	let got = cdi_for(
		"services:\n  app:\n    image: x\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - capabilities: [gpu]\n              count: 2\n",
	);
	assert_eq!(got, vec!["nvidia.com/gpu=0", "nvidia.com/gpu=1"]);
}

#[test]
fn cdi_gpu_device_ids() {
	let got = cdi_for(
		"services:\n  app:\n    image: x\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - capabilities: [gpu]\n              device_ids: [\"GPU-abc\", \"1\"]\n",
	);
	assert_eq!(got, vec!["nvidia.com/gpu=GPU-abc", "nvidia.com/gpu=1"]);
}

#[test]
fn cdi_top_level_gpus_all() {
	assert_eq!(
		cdi_for("services:\n  app:\n    image: x\n    gpus: all\n"),
		vec!["nvidia.com/gpu=all"]
	);
}

#[test]
fn cdi_top_level_gpus_count() {
	assert_eq!(
		cdi_for("services:\n  app:\n    image: x\n    gpus: 2\n"),
		vec!["nvidia.com/gpu=0", "nvidia.com/gpu=1"]
	);
}

#[test]
fn cdi_top_level_gpus_device_list() {
	assert_eq!(
		cdi_for(
			"services:\n  app:\n    image: x\n    gpus:\n      - capabilities: [gpu]\n        device_ids: [\"GPU-xyz\"]\n",
		),
		vec!["nvidia.com/gpu=GPU-xyz"]
	);
}

#[test]
fn cdi_non_gpu_skipped() {
	let got = cdi_for(
		"services:\n  app:\n    image: x\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - capabilities: [tpu]\n              driver: google\n",
	);
	assert!(got.is_empty());
}

#[test]
fn cdi_absent_without_deploy() {
	assert!(cdi_devices(&default_service()).is_empty());
}

// --- ulimit value conversion ---

#[test]
fn ulimit_minus_one_is_unlimited() {
	assert_eq!(ulimit_value(-1, "nofile", "soft"), u64::MAX);
}

#[test]
fn ulimit_other_negative_clamped_to_zero() {
	// Must not wrap to a huge u64 via `as`.
	assert_eq!(ulimit_value(-5, "nofile", "soft"), 0);
}

#[test]
fn ulimit_positive_passes_through() {
	assert_eq!(ulimit_value(1024, "nofile", "hard"), 1024);
}

// --- gpu count clamp ---

#[test]
fn cdi_gpu_count_is_clamped() {
	let yaml = format!(
		"services:\n  g:\n    image: x\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - capabilities: [gpu]\n              count: {}\n",
		MAX_GPU_DEVICES + 10_000
	);
	let file = crate::compose::parse_str(&yaml).unwrap();
	let out = cdi_devices(&file.services["g"]);
	assert_eq!(out.len(), MAX_GPU_DEVICES as usize);
}
