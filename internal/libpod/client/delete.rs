use bytes::Bytes;
use hyper::Method;

use super::{full, Client, Result, READ_TIMEOUT};

impl Client {
	/// `DELETE` → `Ok(true)` if the resource existed and was removed, `Ok(false)`
	/// on a 404.
	pub async fn delete_existed(&self, path: &str) -> Result<bool> {
		let req = Self::build_request(Method::DELETE, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		if status == hyper::StatusCode::NOT_FOUND {
			return Ok(false);
		}
		Self::check_status(status, &body)?;
		Ok(true)
	}

	/// `DELETE` → ignore response body (expect 2xx or 404).
	pub async fn delete_ok(&self, path: &str) -> Result<()> {
		self.delete_existed(path).await.map(|_| ())
	}
}
