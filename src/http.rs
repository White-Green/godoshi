pub use crate::core::http::HttpClient;
use axum::http::{Request, Response};
use oauth2::AsyncHttpClient;
use std::pin::Pin;

pub struct OAuthHttpClient<'a, C>(pub &'a C);

impl<'a, 'c, C> AsyncHttpClient<'c> for OAuthHttpClient<'a, C>
where
    C: HttpClient,
{
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Vec<u8>>, Self::Error>> + Send + 'c>>;

    fn call(&'c self, request: Request<Vec<u8>>) -> Self::Future {
        Box::pin(HttpClient::call(self.0, request))
    }
}
