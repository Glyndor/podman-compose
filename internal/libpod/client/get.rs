use bytes::Bytes;
use hyper::body::Incoming;
use serde::de::DeserializeOwned;

use super::{full, Client, Result, READ_TIMEOUT};

impl Client {
	/// `GET` → deserialize JSON response.
	pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let req = Self::build_request(hyper::Method::GET, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(super::PodmanError::Json)
	}

	/// `GET` → return raw `Response<Incoming>` for streaming.
	pub async fn get_stream(&self, path: &str) -> Result<hyper::Response<Incoming>> {
		let req = Self::build_request(hyper::Method::GET, path, full(Bytes::new()), None)?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}
}
