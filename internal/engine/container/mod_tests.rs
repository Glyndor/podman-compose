use super::rootless_caveat_warnings;
use crate::compose::types::Service;

#[test]
fn no_caveat_warnings_for_plain_service() {
	assert!(rootless_caveat_warnings("web", &Service::default()).is_empty());
}

#[test]
fn warns_for_each_rootless_caveat_field() {
	let service = Service {
		privileged: Some(true),
		oom_kill_disable: Some(true),
		mem_swappiness: Some(10),
		cpu_rt_runtime: Some(1000),
		links: vec!["db".into()],
		external_links: vec!["legacy_db:db".into()],
		..Service::default()
	};
	let warnings = rootless_caveat_warnings("web", &service);
	assert_eq!(warnings.len(), 6);
	let joined = warnings.join("\n");
	for needle in [
		"privileged",
		"oom_kill_disable",
		"mem_swappiness",
		"cpu_rt_runtime",
		"links",
		"external_links",
	] {
		assert!(joined.contains(needle), "missing warning for {needle}");
	}
}
