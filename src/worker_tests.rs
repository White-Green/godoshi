#[cfg(all(target_arch = "wasm32", not(panic = "unwind")))]
compile_error!("worker-tests requires worker-build --panic-unwind");

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::FutureExt;
use serde::Serialize;
use std::any::{Any, type_name};
use std::panic::AssertUnwindSafe;
use worker::Env;

pub struct TestContext {
    env: Env,
    results: Vec<TestResult>,
}

#[derive(Serialize)]
struct TestResult {
    name: &'static str,
    status: TestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TestStatus {
    Passed,
    Failed,
}

impl TestContext {
    pub async fn run<F>(&mut self, test: F)
    where
        F: AsyncFnOnce(Env),
    {
        let name = type_name::<F>();
        worker::console_log!("test {name} ...");
        let env = self.env.clone();
        let result = AssertUnwindSafe(async move {
            reset_database(&env).await;
            test(env).await;
        })
        .catch_unwind()
        .await;

        let (status, error) = match result {
            Ok(()) => (TestStatus::Passed, None),
            Err(payload) => (TestStatus::Failed, Some(panic_message(payload.as_ref()))),
        };
        worker::console_log!("test {name}: {status:?}");
        self.results.push(TestResult { name, status, error });
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        "non-string panic payload".to_owned()
    }
}

async fn reset_database(env: &Env) {
    // This binding exists only in the local test runner, independently of the application's DB.
    let db = env.d1("TEST_DB").unwrap();
    let scripts = [
        "DROP TABLE IF EXISTS author_metadata; DROP TABLE IF EXISTS project_metadata_schema;
         DROP TABLE IF EXISTS environment_upload; DROP TABLE IF EXISTS resource_file_list; DROP TABLE IF EXISTS s3_multipart_part; DROP TABLE IF EXISTS s3_multipart_upload;
         DROP TABLE IF EXISTS permission; DROP TABLE IF EXISTS project_member;
         DROP TABLE IF EXISTS discord_user; DROP TABLE IF EXISTS user; DROP TABLE IF EXISTS project;",
        include_str!("../migrations/0001_initial.sql"),
        include_str!("../migrations/0002_setup.sql"),
        include_str!("../migrations/0003_s3_multipart.sql"),
        include_str!("../migrations/0004_project_member.sql"),
        include_str!("../migrations/0005_resource_files.sql"),
        include_str!("../migrations/0006_environment_upload.sql"),
        include_str!("../migrations/0007_author_metadata.sql"),
        include_str!("../migrations/0008_build_revision.sql"),
    ];
    let statements = scripts
        .into_iter()
        .flat_map(|script| script.split(';'))
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .map(|sql| db.prepare(sql))
        .collect();
    db.batch(statements).await.unwrap();
}

pub async fn run(env: Env) -> Response {
    let mut context = TestContext { env, results: Vec::new() };
    crate::infrastructure::d1::tests::run(&mut context).await;
    crate::infrastructure::d1::discord::tests::run(&mut context).await;
    crate::infrastructure::bucket::tests::run(&mut context).await;

    let passed = context.results.iter().filter(|result| result.status == TestStatus::Passed).count();
    let failed = context.results.len() - passed;
    let status = if failed == 0 {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (
        status,
        Json(serde_json::json!({
            "passed": passed,
            "failed": failed,
            "tests": context.results,
        })),
    )
        .into_response()
}
