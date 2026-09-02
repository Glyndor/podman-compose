use super::attach_log_query;

#[test]
fn attach_query_suppresses_log_backlog() {
	// `tail=0` means attach streams live output only, not the full history.
	let q = attach_log_query();
	assert!(q.contains("follow=true"), "got: {q}");
	assert!(q.contains("tail=0"), "got: {q}");
}
