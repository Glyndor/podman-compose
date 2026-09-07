use super::size_cell;
#[cfg(unix)]
use crate::compose::types::Service;
#[cfg(unix)]
use crate::engine::fake_podman;
#[cfg(unix)]
use crate::engine::Engine;

/// The exact strings the reference tools printed for these byte counts on
/// 2026-08-03: `docker compose` v5.1.3 rendered `98.2MB` for `redis:8-alpine`,
/// and `podman images` rendered `1.01 GB` and `805 kB` on the same host. The
/// table exists to be compared against theirs, so a divergence here is a bug in
/// this column rather than a matter of taste.
#[test]
fn the_size_cell_matches_the_reference_tools() {
	assert_eq!(size_cell(98_234_179), "98.2MB");
	assert_eq!(size_cell(805_007), "805kB");
	assert_eq!(size_cell(1_010_000_000), "1.01GB");
}

/// Decimal, not binary. The same image renders 5% smaller under the binary
/// ladder, and a reader diffing this table against `podman images` would see
/// every row disagree.
#[test]
fn the_size_cell_uses_the_decimal_ladder() {
	// 98234179 bytes is 98.2 MB decimal and 93.7 MiB binary.
	let cell = size_cell(98_234_179);
	assert!(
		cell.ends_with("MB"),
		"{cell:?} is not on the decimal ladder"
	);
	assert!(!cell.contains("iB"), "{cell:?} used a binary unit");
}

/// An image that is not present locally reports zero, and zero is not a size:
/// it is the absence of an answer. An empty cell says that; `0B` would claim
/// podup asked and the image really is empty.
#[test]
fn a_missing_image_leaves_the_cell_empty() {
	assert_eq!(size_cell(0), "");
}

/// One byte is a real size and renders as one, so the empty cell above is
/// keyed on "no answer" and not on "small".
#[test]
fn a_one_byte_image_still_renders() {
	assert_eq!(size_cell(1), "1B");
}

#[cfg(unix)]
fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

/// #1742: `podup images` used to walk services sequentially, issuing one
/// `GET /images/{ref}/json` per service, even when several services share an
/// image. The fix dedupes by image reference so several services on one image
/// cost one inspect, the same shape `resolve_image_digests` already follows
/// when a `config --resolve-image-digests` call asks for the same thing.
/// The
/// request count, not the row count, is what catches a regression here: the
/// rows come back identical whether the listing is fetched once per service
/// or once per unique reference.
#[tokio::test]
#[cfg(unix)]
async fn images_inspects_each_unique_image_reference_once() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/images/shared/json") {
			(
				200,
				r#"{"Id":"sha256:1111111111111111111111111111111111111111111111111111111111111111","Size":1,"Created":"2026-01-01T00:00:00Z"}"#
					.to_string(),
			)
		} else if method == "GET" && target.contains("/images/other/json") {
			(
				200,
				r#"{"Id":"sha256:2222222222222222222222222222222222222222222222222222222222222222","Size":2,"Created":"2026-01-01T00:00:00Z"}"#
					.to_string(),
			)
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = crate::compose::types::ComposeFile::default();
	for name in ["a", "b", "c"] {
		let svc = Service {
			image: Some("shared".into()),
			..Service::default()
		};
		file.services.insert(name.into(), svc);
	}
	let other = Service {
		image: Some("other".into()),
		..Service::default()
	};
	file.services.insert("d".into(), other);

	e.images_with_services(&file, &[], super::super::super::ImagesOptions::default())
		.await
		.expect("images listing over present tags must succeed");

	let seen = fake.requests.lock().unwrap();
	let inspects = seen
		.iter()
		.filter(|r| r.starts_with("GET") && r.contains("/images/") && r.contains("/json"))
		.count();
	assert_eq!(
		inspects, 2,
		"three services on one image plus one on another must inspect two unique images, \
		 not four: requests were {seen:?}"
	);
	let shared_inspects = seen
		.iter()
		.filter(|r| r.contains("GET") && r.contains("/images/shared/json"))
		.count();
	assert_eq!(
		shared_inspects, 1,
		"the shared image must be inspected once across the three services, \
		 not once per service: requests were {seen:?}"
	);
}
