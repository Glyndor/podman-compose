use super::*;
use crate::libpod::Client;

fn engine(project: &str) -> Engine {
	Engine::with_base_dir(
		Client::new("/nonexistent.sock"),
		project.into(),
		std::env::temp_dir(),
	)
}

#[test]
fn lock_acquire_release_reacquire() {
	let e = engine("podup-locktest");
	let first = e.lock_project().expect("first acquire");
	drop(first);
	// Dropping the guard must release the flock so a fresh acquire succeeds.
	let _second = e.lock_project().expect("re-acquire after release");
}

#[test]
fn lock_rejects_unsafe_project_name() {
	assert!(engine("../evil").lock_project().is_err());
	assert!(engine(".hidden").lock_project().is_err());
}

#[test]
fn second_holder_blocks_until_first_releases() {
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::sync::{Arc, Barrier};
	use std::thread;

	// Two engines contend for the same project lock. The first holds it; the
	// second must take the blocking `LOCK_EX` path in `acquire` and only
	// succeed after the first guard is dropped. `released` proves the order.
	let project = "podup-lock-contention";
	let held = engine(project).lock_project().expect("first acquire");

	let released = Arc::new(AtomicBool::new(false));
	let flag = Arc::clone(&released);
	// A rendezvous removes the need for a timing sleep: both threads clear the
	// barrier, then the waiter immediately enters the blocking acquire path.
	let barrier = Arc::new(Barrier::new(2));
	let waiter_barrier = Arc::clone(&barrier);
	let waiter = thread::spawn(move || {
		waiter_barrier.wait();
		let _guard = engine(project).lock_project().expect("second acquire");
		assert!(
			flag.load(Ordering::SeqCst),
			"second holder acquired the lock before the first released it"
		);
	});

	// The lock is exclusive, so the waiter cannot acquire until `held` is
	// dropped; the store is sequenced before the drop, so the waiter is
	// guaranteed to observe `released == true`. This is deterministic — it
	// never depends on how long the waiter takes to reach `flock`.
	barrier.wait();
	released.store(true, Ordering::SeqCst);
	drop(held);

	waiter.join().expect("waiter thread panicked");
}
