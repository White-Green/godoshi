// App/auth/project depend on core ports. Runtime composes them with infrastructure.
// Native tests and UI previews use test_support; worker-tests exercise only concrete adapters.
mod app;
mod auth;
pub mod core;
mod encrypted_cookie;
mod http;
mod project;
mod s3;

mod infrastructure;
mod runtime;
#[cfg(feature = "worker-tests")]
mod test_db;
#[cfg(feature = "worker-tests")]
mod worker_tests;

#[cfg(any(test, feature = "ui-preview"))]
mod test_support;
#[cfg(feature = "ui-preview")]
mod ui_preview;
