use super::*;

fn svc_with_security(opts: &[&str]) -> Service {
	Service {
		security_opt: opts.iter().map(|s| s.to_string()).collect(),
		..Default::default()
	}
}

#[test]
fn security_opts_decompose_each_kind() {
	// Compose colon-form and equals-form both parse.
	let svc = svc_with_security(&[
		"no-new-privileges:true",
		"label=type:container_t",
		"apparmor:my-profile",
		"seccomp=unconfined",
		"mask=/proc/kcore:/proc/timer_list",
		"unmask:ALL",
	]);
	let s = parse_security_opts(&svc);
	assert_eq!(s.no_new_privileges, Some(true));
	assert_eq!(s.selinux_opts, vec!["type:container_t".to_string()]);
	assert_eq!(s.apparmor_profile.as_deref(), Some("my-profile"));
	assert_eq!(s.seccomp_profile_path.as_deref(), Some("unconfined"));
	// mask is colon-split like Podman's own parser; unmask is kept whole.
	assert_eq!(
		s.mask,
		vec!["/proc/kcore".to_string(), "/proc/timer_list".to_string()]
	);
	assert_eq!(s.unmask, vec!["ALL".to_string()]);
}

#[test]
fn no_new_privileges_bare_is_true_and_false_parses() {
	assert_eq!(
		parse_security_opts(&svc_with_security(&["no-new-privileges"])).no_new_privileges,
		Some(true)
	);
	assert_eq!(
		parse_security_opts(&svc_with_security(&["no-new-privileges=false"])).no_new_privileges,
		Some(false)
	);
}

#[test]
fn unknown_security_opt_is_skipped_not_panicked() {
	let s = parse_security_opts(&svc_with_security(&["proc-opts=nosuid"]));
	assert!(s.selinux_opts.is_empty() && s.apparmor_profile.is_none());
}

#[test]
fn device_cgroup_rule_parses_numbers_and_wildcards() {
	let r = parse_device_cgroup_rule("c 1:3 rwm").unwrap();
	assert!(r.allow);
	assert_eq!(r.device_type.as_deref(), Some("c"));
	assert_eq!(r.major, Some(1));
	assert_eq!(r.minor, Some(3));
	assert_eq!(r.access.as_deref(), Some("rwm"));

	let wild = parse_device_cgroup_rule("a *:* rwm").unwrap();
	assert_eq!(wild.major, None);
	assert_eq!(wild.minor, None);
}

#[test]
fn malformed_device_cgroup_rule_is_none() {
	assert!(parse_device_cgroup_rule("c 1:3").is_none()); // missing access
	assert!(parse_device_cgroup_rule("c 13 rwm").is_none()); // no major:minor split
	assert!(parse_device_cgroup_rule("c x:3 rwm").is_none()); // non-numeric, non-*
	assert!(parse_device_cgroup_rule("c 1:3 rwm extra").is_none()); // too many fields
}

#[test]
fn cdi_device_carries_name_as_path() {
	let d = cdi_device("nvidia.com/gpu=all".to_string());
	assert_eq!(d.path, "nvidia.com/gpu=all");
	assert_eq!(d.major, 0);
	assert_eq!(d.minor, 0);
}
