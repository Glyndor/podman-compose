use bytes::Bytes;
use hyper::Method;

use super::{full, Client, Result, READ_TIMEOUT};

impl Client {
	/// `PUT` with raw bytes body → expect 2xx.
	pub async fn put_bytes_ok(&self, path: &str, bytes: Bytes, content_type: &str) -> Result<()> {
		let len = bytes.len();
		let req = Self::build_request(Method::PUT, path, full(bytes), Some(content_type))?;
		let resp = match self.send(req, Some(READ_TIMEOUT)).await {
			Ok(r) => r,
			Err(e) => {
				tracing::debug!(
					"PUT {path} ({content_type}, {len} bytes) ended [{}]: {e}",
					e.stream_end_kind()
				);
				return Err(e);
			}
		};
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)
	}
}
