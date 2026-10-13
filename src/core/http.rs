use axum::http::{Request, Response};
use std::error::Error;

mod file;
pub use file::FileResponse;

pub trait HttpClient: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn call(&self, request: Request<Vec<u8>>) -> impl Future<Output = Result<Response<Vec<u8>>, Self::Error>> + Send + '_;
}
