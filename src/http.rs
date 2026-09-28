use axum::http::{Request, Response};
use oauth2::AsyncHttpClient;
use std::error::Error;
use std::pin::Pin;

pub trait HttpClient {
    type Error: Error + 'static;

    fn call(&self, request: Request<Vec<u8>>) -> impl Future<Output = Result<Response<Vec<u8>>, Self::Error>> + '_;
}

pub struct OAuthHttpClient<'a, C>(pub &'a C);

impl<'a, 'c, C> AsyncHttpClient<'c> for OAuthHttpClient<'a, C>
where
    C: HttpClient,
{
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Vec<u8>>, Self::Error>> + 'c>>;

    fn call(&'c self, request: Request<Vec<u8>>) -> Self::Future {
        Box::pin(HttpClient::call(self.0, request))
    }
}
