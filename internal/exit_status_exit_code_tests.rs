use super::command_failure_exit_code;

#[test]
fn not_found_maps_to_127() {
	assert_eq!(
		command_failure_exit_code(
			"podman error: crun: executable file `foo` not found in $PATH: \
			 No such file or directory: OCI runtime command not found error"
		),
		127
	);
	assert_eq!(
		command_failure_exit_code("OCI runtime error: ...: no such file or directory"),
		127
	);
}

#[test]
fn not_executable_maps_to_126() {
	assert_eq!(
		command_failure_exit_code("OCI runtime error: permission denied"),
		126
	);
	assert_eq!(command_failure_exit_code("exec format error"), 126);
}

#[test]
fn unrelated_errors_map_to_1() {
	assert_eq!(command_failure_exit_code("some other failure"), 1);
	assert_eq!(command_failure_exit_code("container is restarting"), 1);
}
