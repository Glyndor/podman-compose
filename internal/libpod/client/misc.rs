use bytes::Bytes;
use hyper::StatusCode;

use super::{full, meets_minimum, Client, PathStat, Result, READ_TIMEOUT};

impl Client {
	/// `GET /libpod/_ping` — returns Ok(()) when Podman is reachable and
	/// speaks a supported libpod API version.
	pub async fn ping(&self) -> Result<()> {
		let req = Self::build_request(
			hyper::Method::GET,
			"/libpod/_ping",
			full(Bytes::new()),
			None,
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let reported = resp
			.headers()
			.get("Libpod-API-Version")
			.and_then(|v| v.to_str().ok())
			.unwrap_or_default()
			.to_owned();
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		if !meets_minimum(&reported) {
			return Err(super::PodmanError::IncompatibleApiVersion { reported });
		}
		Ok(())
	}

	/// `HEAD` a container-archive path and decode its stat header.
	async fn head_container_path_stat(&self, path: &str) -> Result<Option<PathStat>> {
		use base64::Engine as _;

		let req = Self::build_request(hyper::Method::HEAD, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let status = resp.status();
		if status == StatusCode::NOT_FOUND {
			return Ok(None);
		}
		let stat = resp
			.headers()
			.get("X-Docker-Container-Path-Stat")
			.and_then(|v| v.to_str().ok())
			.map(str::to_string);
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		if status == StatusCode::NOT_FOUND {
			return Ok(None);
		}
		Self::check_status(status, &body)?;
		let Some(stat) = stat else {
			return Ok(Some(PathStat::default()));
		};
		let json = base64::engine::general_purpose::STANDARD
			.decode(stat.as_bytes())
			.map_err(|e| super::PodmanError::Api {
				status: 0,
				message: format!("malformed container path stat: {e}"),
			})?;
		Ok(Some(
			serde_json::from_slice(&json).map_err(super::PodmanError::Json)?,
		))
	}

	/// `HEAD` a container-archive path, returning whether the path is a directory.
	pub async fn head_path_is_dir(&self, path: &str) -> Result<Option<bool>> {
		Ok(self
			.head_container_path_stat(path)
			.await?
			.map(|s| s.mode & (1 << 31) != 0))
	}

	/// The full decoded stat for a container path, or `None` when it does not exist.
	pub(crate) async fn head_path_stat(&self, path: &str) -> Result<Option<PathStat>> {
		self.head_container_path_stat(path).await
	}
}
