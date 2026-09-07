use bytes::Bytes;
use futures_util::Stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Method, Response};
use serde::{de::DeserializeOwned, Serialize};

use super::{full, Client, Result, READ_TIMEOUT};

impl Client {
	/// `POST` with JSON body → deserialize JSON response.
	pub async fn post_json<B: Serialize, T: DeserializeOwned>(
		&self,
		path: &str,
		body: &B,
	) -> Result<T> {
		let json = serde_json::to_vec(body).map_err(super::PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(super::PodmanError::Json)
	}

	/// `POST` with JSON body → ignore response body (expect 2xx).
	pub async fn post_json_ok<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
		let json = serde_json::to_vec(body).map_err(super::PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)
	}

	/// `POST` with JSON body → deserialize JSON response, promoting a 4xx/5xx
	/// to a [`PodmanError::Field`](crate::libpod::PodmanError::Field) when
	/// `field` names a compose-side key the
	/// caller knows was being attempted.
	///
	/// Prefer this over [`post_json`](Self::post_json) at call sites where a
	/// single field is in scope: the error then reads `field: <libpod message>
	/// (value: <value>)` instead of the generic HTTP framing, so the operator
	/// sees what podup was trying to set. The libpod message is preserved
	/// inside the `Field` so the cause is not lost (#1357).
	pub async fn post_json_with_field<B, T>(
		&self,
		path: &str,
		body: &B,
		field: Option<(&'static str, &str)>,
	) -> Result<T>
	where
		B: Serialize,
		T: DeserializeOwned,
	{
		let json = serde_json::to_vec(body).map_err(super::PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status_with_field(status, &body, field)?;
		serde_json::from_slice(&body).map_err(super::PodmanError::Json)
	}

	/// `POST` with JSON body → ignore response body, promoting a 4xx/5xx to a
	/// [`PodmanError::Field`](crate::libpod::PodmanError::Field) when `field`
	/// names a compose-side key. See
	/// [`post_json_with_field`](Self::post_json_with_field).
	pub async fn post_json_ok_with_field<B>(
		&self,
		path: &str,
		body: &B,
		field: Option<(&'static str, &str)>,
	) -> Result<()>
	where
		B: Serialize,
	{
		let json = serde_json::to_vec(body).map_err(super::PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status_with_field(status, &body, field)
	}

	/// `POST` with JSON body → return raw `Response<Incoming>` for streaming.
	pub async fn post_json_stream<B: Serialize>(
		&self,
		path: &str,
		body: &B,
	) -> Result<Response<Incoming>> {
		let json = serde_json::to_vec(body).map_err(super::PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with empty body → ignore response body (expect 2xx or 304).
	pub async fn post_empty_ok(&self, path: &str) -> Result<()> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		if status == hyper::StatusCode::NOT_MODIFIED {
			return Ok(());
		}
		Self::check_status(status, &body)
	}

	/// `POST` with empty body → ignore response body (expect 2xx or 304), bounded
	/// by a caller-chosen deadline rather than the default `READ_TIMEOUT`.
	pub async fn post_empty_ok_within(
		&self,
		path: &str,
		deadline: Option<std::time::Duration>,
	) -> Result<()> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, deadline).await?;
		let (status, body) = resp.read_body(deadline).await?;
		if status == hyper::StatusCode::NOT_MODIFIED {
			return Ok(());
		}
		Self::check_status(status, &body)
	}

	/// `POST` with JSON body → return raw `Response<Incoming>` for streaming,
	/// bounding the wait for the response head by `head_timeout`.
	pub async fn post_json_stream_within<B: Serialize>(
		&self,
		path: &str,
		body: &B,
		head_timeout: Option<std::time::Duration>,
	) -> Result<Response<Incoming>> {
		let json = serde_json::to_vec(body).map_err(super::PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		Self::stream_or_err(self.send_streaming(req, head_timeout).await?).await
	}

	/// `POST` with empty body → return raw `Response<Incoming>` for streaming.
	pub async fn post_empty_stream(&self, path: &str) -> Result<Response<Incoming>> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with empty body → deserialize JSON response.
	pub async fn post_empty_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(super::PodmanError::Json)
	}

	/// `POST` with empty body → deserialize JSON response without a read timeout.
	pub async fn post_empty_json_unbounded<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, None).await?;
		let (status, body) = resp.read_body(None).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(super::PodmanError::Json)
	}

	/// `POST` with raw bytes body → return raw `Response<Incoming>` for streaming.
	pub async fn post_bytes_stream(
		&self,
		path: &str,
		bytes: Bytes,
		content_type: &str,
	) -> Result<Response<Incoming>> {
		let req = Self::build_request(Method::POST, path, full(bytes), Some(content_type))?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with a streamed body → return raw `Response<Incoming>` for streaming.
	pub async fn post_stream_body<S>(
		&self,
		path: &str,
		chunks: S,
		content_type: &str,
	) -> Result<Response<Incoming>>
	where
		S: Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send + 'static,
	{
		let body = StreamBody::new(chunks).boxed_unsync();
		let req = Self::build_request(Method::POST, path, body, Some(content_type))?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with a raw-bytes body → deserialize JSON response.
	pub async fn post_bytes_json<T: DeserializeOwned>(
		&self,
		path: &str,
		bytes: Bytes,
		content_type: &str,
	) -> Result<T> {
		let req = Self::build_request(Method::POST, path, full(bytes), Some(content_type))?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = resp.read_body(Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(super::PodmanError::Json)
	}
}
