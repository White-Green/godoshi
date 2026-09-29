use axum::extract::FromRef;
use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::Key;

#[derive(Debug, Clone)]
pub struct EncryptedCookieKey(Key);

impl EncryptedCookieKey {
    pub fn from_bytes(key: &[u8]) -> Self {
        EncryptedCookieKey(Key::derive_from(key))
    }
}

impl From<EncryptedCookieKey> for Key {
    fn from(key: EncryptedCookieKey) -> Self {
        key.0
    }
}

pub type EncryptedCookieJar = PrivateCookieJar<EncryptedCookieKey>;

pub trait EncryptedCookieKeyProvider {
    fn encrypted_cookie_key(&self) -> EncryptedCookieKey;
}

impl<P: EncryptedCookieKeyProvider> FromRef<P> for EncryptedCookieKey {
    fn from_ref(input: &P) -> Self {
        input.encrypted_cookie_key()
    }
}
