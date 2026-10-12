use super::notice::{self, Notices};
use super::route;
use crate::auth::AuthState;
use crate::config::GenericConfigProvider;
use crate::project::{ProjectName, ProjectState};
use crate::test_db::discord_user::TestDiscordUser;
use crate::test_db::project::TestProject;
use crate::test_db::project_member::TestProjectMember;
use crate::test_db::{discord_user, project, project_member};
use crate::user::UserPermissionSet;
use crate::worker_tests::TestContext;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Json, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use futures::FutureExt;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use std::panic::AssertUnwindSafe;
use tower_service::Service;
use worker::{Bucket, D1Database, Env};

pub async fn run(context: &mut TestContext) {
    context.run(notice_extensions_round_trip).await;
    context.run(admin_adds_and_reuses_users).await;
    context.run(admin_updates_project_permissions).await;
    context.run(admin_rejects_ambiguous_users).await;
    context.run(admin_displays_project_usernames).await;
    context.run(pages_require_matching_permissions).await;
    context.run(forms_require_same_origin).await;
    context.run(root_links_and_redirects).await;
    context.run(review_lists_and_streams_project_results).await;
    context.run(review_displays_run_datetimes).await;
    context.run(review_groups_runs_in_ascending_order).await;
    context.run(review_lists_all_run_files_in_descending_order).await;
    context.run(participant_uploads_and_replaces_files).await;
    context.run(participant_rejects_invalid_uploads).await;
    context.run(participant_lists_and_streams_own_results).await;
    context.run(participant_lists_all_own_results).await;
    context.run(participant_results_panic_on_invalid_username).await;
}

#[derive(Clone)]
struct TestConfig(Env);

impl GenericConfigProvider for TestConfig {
    fn base_url(&self) -> String {
        "https://godoshi.test".into()
    }
    fn db(&self) -> D1Database {
        self.0.d1("TEST_DB").unwrap()
    }
    fn bucket(&self) -> Bucket {
        self.0.bucket("TEST_BUCKET").unwrap()
    }
}

async fn setup(env: Env, permissions: UserPermissionSet) -> (TestConfig, AuthState) {
    let config = TestConfig(env);
    let db = config.db();
    let project_id = project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await;
    project::insert(&db, TestProject { id: 2, name: "beta".into() }).await;
    discord_user::insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: Some("12345".into()),
            username: "owner".into(),
        },
    )
    .await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "owner".into(),
            permission_set: permissions.bits().into(),
        },
    )
    .await;
    let auth = AuthState {
        user_id: serde_json::from_str("1").unwrap(),
        username: "owner".into(),
        project: ProjectState {
            id: project_id,
            name: ProjectName::new("alpha".into()).unwrap(),
        },
        permission_set: permissions,
    };
    let bucket = config.bucket();
    loop {
        let objects = bucket.list().execute().await.unwrap().objects();
        if objects.is_empty() {
            break;
        }
        bucket.delete_multiple(objects.iter().map(|object| object.key()).collect()).await.unwrap();
    }
    (config, auth)
}

async fn call(config: &TestConfig, auth: &AuthState, request: Request<Body>) -> Response {
    let mut router = Router::new().nest_service(
        "/{project}",
        route(config.clone())
            .layer(Extension(auth.clone()))
            .layer(Extension(auth.project.clone())),
    );
    router.call(request).await.unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("https://godoshi.test/alpha{path}"))
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, form: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("https://godoshi.test/alpha{path}"))
        .header("origin", "https://godoshi.test")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form.to_owned()))
        .unwrap()
}

async fn body(response: Response) -> String {
    String::from_utf8(to_bytes(response.into_body(), 1024 * 1024).await.unwrap().to_vec()).unwrap()
}

async fn assert_saved(response: Response) -> String {
    if response.status() != StatusCode::SEE_OTHER {
        panic!("Expected a successful redirect, got {}: {}", response.status(), body(response).await);
    }
    let location = response.headers()["location"].to_str().unwrap();
    assert!(!location.contains('?'));
    assert_eq!(response.headers().get_all("set-cookie").iter().count(), 1);
    let cookie = Cookie::parse(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
    assert_eq!(cookie.name(), "godoshi_notice");
    assert_eq!(cookie.path(), Some("/alpha"));
    let decoded = Cookie::parse_encoded(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
    let payload: serde_json::Value = serde_json::from_str(decoded.value()).unwrap();
    assert_eq!(payload["path"], location);
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(cookie.secure(), Some(true));
    assert_eq!(cookie.same_site(), Some(SameSite::Lax));
    assert_eq!(cookie.max_age(), Some(time::Duration::minutes(5)));
    format!("{}={}", cookie.name(), cookie.value())
}

async fn consume_notice(config: &TestConfig, auth: &AuthState, path: &str, cookie: &str, message: &str) -> String {
    let response = call(config, auth, with_cookie(get(path), cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get_all("set-cookie").iter().count(), 1);
    let removed = Cookie::parse(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
    assert_eq!(removed.name(), "godoshi_notice");
    assert_eq!(removed.path(), Some("/alpha"));
    assert_eq!(removed.max_age(), Some(time::Duration::ZERO));
    let html = body(response).await;
    assert!(html.contains(message));
    // Reloading after the cookie is cleared, or supplying the old query flags,
    // must not display a success notification.
    for path in [path.to_owned(), format!("{path}?saved=true&uploaded=true")] {
        let response = call(config, auth, get(&path)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!body(response).await.contains(message));
    }
    html
}

fn with_cookie(mut request: Request<Body>, cookie: &str) -> Request<Body> {
    request.headers_mut().insert("cookie", cookie.parse().unwrap());
    request
}

async fn echo_notices(request: Request<Body>) -> Json<Notices> {
    Json(request.extensions().get::<Notices>().unwrap().clone())
}

async fn notice_extensions_round_trip(_env: Env) {
    let routes = Router::new()
        .route(
            "/send",
            axum::routing::post(async || {
                let mut response = Redirect::to("/alpha/receive").into_response();
                response.extensions_mut().insert(Notices::from([
                    ("saved", serde_json::json!(true)),
                    ("uploaded", serde_json::json!(true)),
                    ("count", serde_json::json!(2)),
                    ("message", serde_json::json!("保存しました。; =\"% +")),
                ]));
                response
            }),
        )
        .route(
            "/receive",
            axum::routing::get(echo_notices).post(async |request: Request<Body>| {
                let mut notices = request.extensions().get::<Notices>().unwrap().clone();
                notices.0.insert("next".into(), serde_json::json!(true));
                (Extension(notices), Redirect::to("/alpha/next?view=all"))
            }),
        )
        .route("/receive/child", axum::routing::get(echo_notices))
        .route("/next", axum::routing::get(echo_notices))
        .route(
            "/local",
            axum::routing::get(echo_notices).post(async || (Extension(Notices::from([("saved", true)])), StatusCode::OK)),
        )
        .route("/", axum::routing::get(echo_notices))
        .route("/empty", axum::routing::get(async || (Extension(Notices::default()), StatusCode::OK)))
        .layer(axum::middleware::from_fn(notice::middleware));
    let mut router = Router::new();
    for name in ["alpha", "beta", "alpha2"] {
        let project = ProjectState {
            id: serde_json::from_str("1").unwrap(),
            name: ProjectName::new(name.into()).unwrap(),
        };
        router = router.nest_service(&format!("/{name}"), routes.clone().layer(Extension(project)));
    }
    let response = router.call(post("/send", "")).await.unwrap();
    assert!(response.extensions().get::<Notices>().is_none());
    let cookie = assert_saved(response).await;

    // The project cookie reaches other pages, but only its exact destination consumes it.
    for path in [
        "/alpha",
        "/alpha/",
        "/alpha/receive/child",
        "/alpha/next",
        "/beta/receive",
        "/alpha2/receive",
    ] {
        let request = Request::builder().uri(format!("https://godoshi.test{path}")).body(Body::empty()).unwrap();
        let response = router.call(with_cookie(request, &cookie)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(body(response).await, "{}");
    }
    let response = router.call(with_cookie(get("/receive"), &cookie)).await.unwrap();
    assert_eq!(response.headers().get_all("set-cookie").iter().count(), 1);
    let removed = Cookie::parse(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
    assert_eq!(removed.path(), Some("/alpha"));
    assert_eq!(removed.max_age(), Some(time::Duration::ZERO));
    let values: Notices = serde_json::from_str(&body(response).await).unwrap();
    assert_eq!(values.0.len(), 4);
    assert_eq!(values.0["saved"], true);
    assert_eq!(values.0["uploaded"], true);
    assert_eq!(values.0["count"], 2);
    assert_eq!(values.0["message"], "保存しました。; =\"% +");
    let response = router.call(get("/receive")).await.unwrap();
    assert!(!response.headers().contains_key("set-cookie"));
    assert_eq!(body(response).await, "{}");

    // Forwarding replaces the project cookie once, with the new destination and all values.
    let response = router.call(with_cookie(post("/receive", ""), &cookie)).await.unwrap();
    let cookies: Vec<_> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| Cookie::parse(value.to_str().unwrap()).unwrap())
        .collect();
    assert_eq!(cookies.len(), 1);
    assert_eq!(cookies[0].path(), Some("/alpha"));
    assert_eq!(cookies[0].max_age(), Some(time::Duration::minutes(5)));
    let forwarded_cookie = format!("{}={}", cookies[0].name(), cookies[0].value());
    let response = router.call(with_cookie(get("/receive"), &forwarded_cookie)).await.unwrap();
    assert!(!response.headers().contains_key("set-cookie"));
    assert_eq!(body(response).await, "{}");
    let response = router.call(with_cookie(get("/next?view=all"), &forwarded_cookie)).await.unwrap();
    assert_eq!(response.headers().get_all("set-cookie").iter().count(), 1);
    let removed = Cookie::parse(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
    assert_eq!(removed.path(), Some("/alpha"));
    assert_eq!(removed.max_age(), Some(time::Duration::ZERO));
    let forwarded: Notices = serde_json::from_str(&body(response).await).unwrap();
    let mut expected = values;
    expected.0.insert("next".into(), serde_json::json!(true));
    assert_eq!(forwarded, expected);

    // Without a redirect, notices target the current page using the same project scope.
    // An unconsumed notice for another page is replaced by the newly emitted notice.
    let response = router.call(with_cookie(post("/local", ""), &cookie)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get_all("set-cookie").iter().count(), 1);
    let cookie = Cookie::parse(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
    assert_eq!(cookie.path(), Some("/alpha"));
    let cookie = format!("{}={}", cookie.name(), cookie.value());
    let response = router.call(with_cookie(get("/local"), &cookie)).await.unwrap();
    let values: Notices = serde_json::from_str(&body(response).await).unwrap();
    assert_eq!(values, Notices::from([("saved", true)]));

    // Invalid/old cookie values are discarded, and an empty extension sets no cookie.
    for invalid in ["saved", "not-json", "%5B%5D"] {
        let response = router
            .call(with_cookie(get("/receive"), &format!("godoshi_notice={invalid}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get_all("set-cookie").iter().count(), 1);
        let removed = Cookie::parse(response.headers()["set-cookie"].to_str().unwrap()).unwrap();
        assert_eq!(removed.path(), Some("/alpha"));
        assert_eq!(removed.max_age(), Some(time::Duration::ZERO));
        assert_eq!(body(response).await, "{}");
    }
    let response = router.call(get("/empty")).await.unwrap();
    assert!(!response.headers().contains_key("set-cookie"));
}

async fn admin_adds_and_reuses_users(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::ADMIN).await;
    assert_saved(
        call(
            &config,
            &auth,
            post("/admin/add_user", "username=new.author&participant=true&reviewer=true"),
        )
        .await,
    )
    .await;
    let db = config.db();
    let users = discord_user::rows(&db).await;
    assert_eq!(users.len(), 2);
    assert_eq!(users[1].username, "new.author");
    assert_eq!(users[1].discord_id, None);
    let permissions = project_member::rows(&db).await;
    assert_eq!(permissions[1].username, "new.author");
    assert_eq!(permissions[1].permission_set, 6);
    let response = call(&config, &auth, post("/admin/add_user", "username=new.author&admin=true")).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(project_member::rows(&db).await, permissions);
    assert_eq!(discord_user::rows(&db).await, users);

    discord_user::insert(
        &db,
        TestDiscordUser {
            id: 3,
            discord_id: Some("67890".into()),
            username: "existing".into(),
        },
    )
    .await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 2,
            user_id: 3,
            username: "existing-in-beta".into(),
            permission_set: 1,
        },
    )
    .await;
    let notice_cookie = assert_saved(call(&config, &auth, post("/admin/add_user", "username=existing&participant=true")).await).await;
    assert_eq!(discord_user::rows(&db).await.len(), 3);
    let permissions = project_member::rows(&db).await;
    assert_eq!(
        permissions[2],
        TestProjectMember {
            project_id: 1,
            user_id: 3,
            username: "existing".into(),
            permission_set: 4
        }
    );
    assert_eq!(
        permissions[3],
        TestProjectMember {
            project_id: 2,
            user_id: 3,
            username: "existing-in-beta".into(),
            permission_set: 1
        }
    );
    let html = consume_notice(&config, &auth, "/admin", &notice_cookie, "保存しました。").await;
    assert!(html.contains("<form action=\"/alpha/admin/update_permission\""));
    assert!(html.contains("name=\"user_id\" value=\"3\""));
    assert!(html.contains("保存しました。"));
    assert!(!html.contains("&lt;form"));
    assert_eq!(
        call(&config, &auth, post("/admin/add_user", "username=%20%20")).await.status(),
        StatusCode::BAD_REQUEST
    );
}

async fn admin_updates_project_permissions(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::ADMIN).await;
    let db = config.db();
    discord_user::insert(
        &db,
        TestDiscordUser {
            id: 2,
            discord_id: None,
            username: "second".into(),
        },
    )
    .await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 2,
            username: "second".into(),
            permission_set: 4,
        },
    )
    .await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 2,
            user_id: 2,
            username: "second-in-beta".into(),
            permission_set: 1,
        },
    )
    .await;
    assert_saved(call(&config, &auth, post("/admin/update_permission", "user_id=2&reviewer=true")).await).await;
    assert_eq!(project_member::rows(&db).await[1].permission_set, 2);
    assert_eq!(project_member::rows(&db).await[2].permission_set, 1);
    assert_saved(call(&config, &auth, post("/admin/update_permission", "user_id=2")).await).await;
    assert_eq!(project_member::rows(&db).await[1].permission_set, 0);
    assert_saved(call(&config, &auth, post("/admin/update_permission", "user_id=1")).await).await;
    assert_eq!(project_member::rows(&db).await[0].permission_set, 1);
    assert_eq!(
        call(&config, &auth, post("/admin/update_permission", "user_id=99&admin=true"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_saved(call(&config, &auth, post("/admin/update_permission", "user_id=2&admin=true")).await).await;
    assert_saved(call(&config, &auth, post("/admin/update_permission", "user_id=1&reviewer=true")).await).await;
    assert_eq!(project_member::rows(&db).await[0].permission_set, 3);
}

async fn admin_rejects_ambiguous_users(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::ADMIN).await;
    let db = config.db();
    for id in [2, 3] {
        discord_user::insert(
            &db,
            TestDiscordUser {
                id,
                discord_id: None,
                username: "duplicate".into(),
            },
        )
        .await;
    }
    let before = project_member::rows(&db).await;
    assert_eq!(
        call(&config, &auth, post("/admin/add_user", "username=duplicate&admin=true"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(project_member::rows(&db).await, before);
}

async fn admin_displays_project_usernames(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::ADMIN).await;
    config
        .db()
        .prepare("UPDATE project_member SET username = 'pen.name' WHERE project_id = 1 AND user_id = 1")
        .run()
        .await
        .unwrap();
    let response = call(&config, &auth, get("/admin")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body(response).await;
    assert!(html.contains("pen.name (1)"));
    assert!(!html.contains("owner (1)"));
    assert_eq!(discord_user::rows(&config.db()).await[0].username, "owner");
}

async fn pages_require_matching_permissions(env: Env) {
    let (config, mut auth) = setup(env, UserPermissionSet::empty()).await;
    for permissions in [
        UserPermissionSet::empty(),
        UserPermissionSet::ADMIN,
        UserPermissionSet::REVIEWER,
        UserPermissionSet::PARTICIPANT,
    ] {
        auth.permission_set = permissions;
        for (path, required) in [
            ("/admin", UserPermissionSet::ADMIN),
            ("/review", UserPermissionSet::REVIEWER),
            ("/review/run", UserPermissionSet::REVIEWER),
            ("/participant", UserPermissionSet::PARTICIPANT),
            ("/participant/run", UserPermissionSet::PARTICIPANT),
        ] {
            let response = call(&config, &auth, get(path)).await;
            assert_eq!(
                response.status(),
                if permissions.contains(required) {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                },
                "{path}"
            );
        }
        if !permissions.contains(UserPermissionSet::ADMIN) {
            assert_eq!(
                call(&config, &auth, post("/admin/add_user", "username=intruder&admin=true"))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                call(&config, &auth, post("/admin/update_permission", "user_id=1&admin=true"))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        if !permissions.contains(UserPermissionSet::REVIEWER) {
            assert_eq!(
                call(&config, &auth, get("/review/run/file?key=test.pdf")).await.status(),
                StatusCode::FORBIDDEN
            );
        }
        if !permissions.contains(UserPermissionSet::PARTICIPANT) {
            assert_eq!(
                call(&config, &auth, get("/participant/run/file?key=test.pdf")).await.status(),
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                call(&config, &auth, multipart("secret.txt", "secret")).await.status(),
                StatusCode::FORBIDDEN
            );
        }
    }
    assert_eq!(discord_user::rows(&config.db()).await.len(), 1);
    assert!(config.bucket().list().execute().await.unwrap().objects().is_empty());
}

async fn forms_require_same_origin(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::all()).await;
    for origin in [Some("https://elsewhere.test"), Some("null"), None] {
        for mut request in [post("/admin/add_user", "username=intruder&admin=true"), multipart("bad.txt", "bad")] {
            request.headers_mut().remove("origin");
            if let Some(origin) = origin {
                request.headers_mut().insert("origin", origin.parse().unwrap());
            }
            assert_eq!(call(&config, &auth, request).await.status(), StatusCode::FORBIDDEN);
        }
    }
    assert_eq!(discord_user::rows(&config.db()).await.len(), 1);
    assert!(config.bucket().list().execute().await.unwrap().objects().is_empty());
}

async fn root_links_and_redirects(env: Env) {
    let (config, mut auth) = setup(env, UserPermissionSet::all()).await;
    let response = call(&config, &auth, get("/")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body(response).await;
    for path in ["admin", "review", "participant"] {
        assert!(html.contains(&format!("href=\"/alpha/{path}\"")));
    }
    for (permissions, path) in [
        (UserPermissionSet::ADMIN, "admin"),
        (UserPermissionSet::REVIEWER, "review"),
        (UserPermissionSet::PARTICIPANT, "participant"),
    ] {
        auth.permission_set = permissions;
        let response = call(&config, &auth, get("/")).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], format!("/alpha/{path}"));
    }
    for permissions in [
        UserPermissionSet::ADMIN,
        UserPermissionSet::REVIEWER,
        UserPermissionSet::PARTICIPANT,
        UserPermissionSet::ADMIN | UserPermissionSet::REVIEWER,
        UserPermissionSet::ADMIN | UserPermissionSet::PARTICIPANT,
        UserPermissionSet::REVIEWER | UserPermissionSet::PARTICIPANT,
        UserPermissionSet::all(),
    ] {
        auth.permission_set = permissions;
        for (path, required) in [
            ("/admin", UserPermissionSet::ADMIN),
            ("/review", UserPermissionSet::REVIEWER),
            ("/participant", UserPermissionSet::PARTICIPANT),
        ] {
            if permissions.contains(required) {
                let response = call(&config, &auth, get(path)).await;
                assert_eq!(response.status(), StatusCode::OK);
                let html = body(response).await;
                assert_eq!(
                    html.contains("href=\"/alpha/\""),
                    permissions.bits().count_ones() >= 2,
                    "{path}: {permissions:?}"
                );
            }
        }
    }
    // Returning from a result to its list is still useful with a single role.
    for (path, permissions) in [("review", UserPermissionSet::REVIEWER), ("participant", UserPermissionSet::PARTICIPANT)] {
        auth.permission_set = permissions;
        let html = body(call(&config, &auth, get(&format!("/{path}/run"))).await).await;
        assert!(html.contains(&format!("href=\"/alpha/{path}\"")));
    }
}

async fn review_lists_and_streams_project_results(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::REVIEWER).await;
    let bucket = config.bucket();
    let filename = "完成 稿&?#%.pdf";
    bucket
        .put(format!("projects/1/results/run/{filename}"), b"%PDF-test".to_vec())
        .execute()
        .await
        .unwrap();
    bucket
        .put("projects/1/results/run/<script>.html", "<script>alert(1)</script>".to_owned())
        .execute()
        .await
        .unwrap();
    bucket
        .put("projects/2/results/run/other.txt", "secret".to_owned())
        .execute()
        .await
        .unwrap();
    bucket
        .put("projects/1/resources/authors/private.txt", "secret".to_owned())
        .execute()
        .await
        .unwrap();
    bucket
        .put("projects/1/results/run-extra/elsewhere.txt", "other run".to_owned())
        .execute()
        .await
        .unwrap();
    let response = call(&config, &auth, get("/review")).await;
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let index = body(response).await;
    assert!(index.contains("href=\"/alpha/review/run\""));
    assert!(!index.contains(".pdf"));
    assert!(!index.contains("other.txt"));
    assert!(!index.contains("private.txt"));
    let response = call(&config, &auth, get("/review/run")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body(response).await;
    assert!(html.contains("<h2>run</h2>"));
    assert!(html.contains("href=\"/alpha/review\""));
    assert!(html.contains("完成 稿&#38;?#%.pdf"));
    assert!(html.contains("&#60;script&#62;.html"));
    assert!(html.find("完成 稿&#38;?#%.pdf").unwrap() < html.find("&#60;script&#62;.html").unwrap());
    assert!(!html.contains("other.txt"));
    assert!(!html.contains("private.txt"));
    assert!(!html.contains("elsewhere.txt"));
    let encoded = utf8_percent_encode(filename, NON_ALPHANUMERIC);
    for (suffix, disposition) in [("", "inline"), ("&download=true", "attachment")] {
        let response = call(&config, &auth, get(&format!("/review/run/file?key={encoded}{suffix}"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/pdf");
        let header = response.headers()["content-disposition"].to_str().unwrap();
        assert!(header.starts_with(disposition));
        assert!(header.contains("filename*=UTF-8''"));
        assert_eq!(body(response).await, "%PDF-test");
    }
    let response = call(&config, &auth, get("/review/run/file?key=%3Cscript%3E.html")).await;
    assert_eq!(response.headers()["content-type"], "text/plain; charset=utf-8");
    assert!(response.headers()["content-security-policy"].to_str().unwrap().contains("sandbox"));
    for key in ["missing", "elsewhere.txt", "run-extra/elsewhere.txt", "other.txt"] {
        assert_eq!(
            call(&config, &auth, get(&format!("/review/run/file?key={key}"))).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    for key in [
        "../resources/authors/private.txt",
        "../run-extra/elsewhere.txt",
        "/projects/2/results/run/other.txt",
        "run%2F..%2Fother.txt",
        "run%5Cother.txt",
    ] {
        assert_eq!(
            call(&config, &auth, get(&format!("/review/run/file?key={key}"))).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    for run in ["%2E%2E", "run%2Fchild", "%2Fprojects%2F2", "run%5Cchild", "run%00"] {
        for suffix in ["", "/file?key=test.txt"] {
            assert_eq!(
                call(&config, &auth, get(&format!("/review/{run}{suffix}"))).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
}

async fn review_displays_run_datetimes(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::REVIEWER).await;
    // The UTC date is January 1; displaying JST must advance to January 2.
    let runs = [
        ("998232709354-2", "2026-01-02 03:04:06 JST"),
        ("998232709355-1", "2026-01-02 03:04:05 JST"),
    ];
    for (run, _) in runs {
        config
            .bucket()
            .put(format!("projects/1/results/{run}/result.txt"), "result".to_owned())
            .execute()
            .await
            .unwrap();
    }
    let html = body(call(&config, &auth, get("/review")).await).await;
    let mut previous = 0;
    for (run, datetime) in runs {
        let encoded = utf8_percent_encode(run, NON_ALPHANUMERIC);
        let link = format!("<a href=\"/alpha/review/{encoded}\">{datetime}</a>");
        let position = html.find(&link).unwrap();
        assert!(previous < position, "Newer runs must appear first");
        previous = position;
        let response = call(&config, &auth, get(&format!("/review/{encoded}"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let detail = body(response).await;
        assert!(detail.contains(&format!("<h2>{datetime}</h2>")));
        assert!(detail.contains(&format!("/review/{encoded}/file?key=result%2Etxt")));
    }
}

async fn review_groups_runs_in_ascending_order(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::REVIEWER).await;
    let bucket = config.bucket();
    for index in 0..101 {
        bucket
            .put(format!("projects/1/results/{index:03}/nested/part.txt"), "result".to_owned())
            .execute()
            .await
            .unwrap();
    }
    let run = "確認 &?#%";
    bucket
        .put(format!("projects/1/results/{run}/one.txt"), "result".to_owned())
        .execute()
        .await
        .unwrap();
    bucket
        .put(format!("projects/1/results/{run}/two.txt"), "result".to_owned())
        .execute()
        .await
        .unwrap();
    let html = body(call(&config, &auth, get("/review")).await).await;
    let encoded = utf8_percent_encode(run, NON_ALPHANUMERIC);
    let link = format!("href=\"/alpha/review/{encoded}\"");
    assert_eq!(html.matches(&link).count(), 1);
    assert!(html.contains("確認 &#38;?#%"));
    let mut previous = 0;
    for index in 0..101 {
        let link = format!("href=\"/alpha/review/{index:03}\"");
        assert_eq!(html.matches(&link).count(), 1);
        let position = html.find(&link).unwrap();
        assert!(previous < position, "Run {index} is out of ascending order");
        previous = position;
    }
    assert!(previous < html.find(&link).unwrap());
    assert!(!html.contains("part.txt"));
    assert!(!html.contains("次のページ"));
    let html = body(call(&config, &auth, get(&format!("/review/{encoded}"))).await).await;
    assert!(html.contains("<h2>確認 &#38;?#%</h2>"));
    assert!(html.find("two.txt").unwrap() < html.find("one.txt").unwrap());
    assert!(!html.contains("part.txt"));
    assert!(html.contains(&format!("href=\"/alpha/review/{encoded}/file?key=one%2Etxt\"")));
    let response = call(&config, &auth, get(&format!("/review/{encoded}/file?key=one%2Etxt"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, "result");
    let html = body(call(&config, &auth, get("/review/100")).await).await;
    assert!(html.contains("nested/part.txt"));
    assert!(!html.contains("one.txt"));
    let response = call(&config, &auth, get("/review/100/file?key=nested%2Fpart.txt")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, "result");
}

async fn review_lists_all_run_files_in_descending_order(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::REVIEWER).await;
    let bucket = config.bucket();
    for index in 0..101 {
        bucket
            .put(format!("projects/1/results/run/{index:03}.txt"), "result".to_owned())
            .execute()
            .await
            .unwrap();
    }
    let html = body(call(&config, &auth, get("/review/run")).await).await;
    let mut previous = 0;
    for index in (0..101).rev() {
        let position = html.find(&format!("<li>{index:03}.txt")).unwrap();
        assert!(previous < position, "File {index} is out of descending order");
        previous = position;
    }
    assert!(html.contains("/review/run/file?key=100%2Etxt"));
    assert!(!html.contains("次のページ"));
}

fn multipart(filename: &str, contents: &str) -> Request<Body> {
    let filename = filename.replace('\\', "\\\\").replace('"', "\\\"");
    let form = format!(
        "--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n{contents}\r\n--boundary--\r\n"
    );
    Request::builder()
        .method("POST")
        .uri("https://godoshi.test/alpha/participant/upload")
        .header("origin", "https://godoshi.test")
        .header("content-type", "multipart/form-data; boundary=boundary")
        .body(Body::from(form))
        .unwrap()
}

async fn participant_uploads_and_replaces_files(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::PARTICIPANT).await;
    let bucket = config.bucket();
    for contents in ["first submission", "replacement"] {
        let response = call(&config, &auth, multipart("原稿 &%.txt", contents)).await;
        assert_saved(response).await;
        let object = bucket.get("projects/1/resources/authors/原稿 &%.txt").execute().await.unwrap().unwrap();
        assert_eq!(object.body().unwrap().text().await.unwrap(), contents);
    }
    assert_eq!(bucket.list().execute().await.unwrap().objects().len(), 1);
    // Manuscripts should not hit Axum's default 2 MiB form limit.
    let contents = "x".repeat(2 * 1024 * 1024 + 1);
    let notice_cookie = assert_saved(call(&config, &auth, multipart("large.txt", &contents)).await).await;
    assert_eq!(
        bucket.head("projects/1/resources/authors/large.txt").await.unwrap().unwrap().size(),
        contents.len() as u64
    );
    let html = consume_notice(&config, &auth, "/participant", &notice_cookie, "アップロードしました。").await;
    assert!(html.contains("enctype=\"multipart/form-data\""));
}

async fn participant_rejects_invalid_uploads(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::PARTICIPANT).await;
    for filename in ["", ".", "..", "../outside.txt", "nested/file.txt", "nested\\file.txt"] {
        assert_eq!(
            call(&config, &auth, multipart(filename, "bad")).await.status(),
            StatusCode::BAD_REQUEST,
            "{filename}"
        );
    }
    assert_eq!(
        call(&config, &auth, post("/participant/upload", "file=not-a-file")).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert!(config.bucket().list().execute().await.unwrap().objects().is_empty());
    let mut request = multipart("large.txt", "");
    let chunk = axum::body::Bytes::from(vec![0; 1024 * 1024]);
    *request.body_mut() = Body::from_stream(futures::stream::iter(
        (0..21).map(move |_| Ok::<_, std::convert::Infallible>(chunk.clone())),
    ));
    assert_eq!(call(&config, &auth, request).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(config.bucket().list().execute().await.unwrap().objects().is_empty());
}

async fn participant_lists_and_streams_own_results(env: Env) {
    let (config, mut auth) = setup(env, UserPermissionSet::PARTICIPANT).await;
    config
        .db()
        .prepare("UPDATE discord_user SET username = 'discord.owner' WHERE id = 1")
        .run()
        .await
        .unwrap();
    let bucket = config.bucket();
    let run = "998232709354-2";
    let encoded_run = utf8_percent_encode(run, NON_ALPHANUMERIC);
    let filename = "nested/完成 稿&?#%.pdf";
    bucket
        .put(format!("projects/1/results/{run}/authors/owner/{filename}"), "%PDF-own".to_owned())
        .execute()
        .await
        .unwrap();
    for (key, contents) in [
        (format!("projects/1/results/{run}/authors/owner/<script>.html"), "<script>own</script>"),
        (format!("projects/1/results/{run}/authors/other/other.pdf"), "other author"),
        (format!("projects/1/results/{run}/authors/discord.owner/discord-only.pdf"), "Discord name"),
        (format!("projects/1/results/{run}/authors/owner-extra/similar.pdf"), "similar username"),
        (format!("projects/1/results/{run}/full.pdf"), "whole project"),
        (format!("projects/2/results/{run}/authors/owner/{filename}"), "other project"),
        ("projects/1/results/998232709355-1/authors/owner/older.txt".into(), "older"),
        ("projects/1/results/998232709353-3/authors/other/other-only.pdf".into(), "other only"),
        ("projects/1/results/998232709352-4/authors/owner/".into(), ""),
        ("projects/1/resources/authors/private.txt".into(), "source"),
    ] {
        bucket.put(key, contents.to_owned()).execute().await.unwrap();
    }
    let response = call(&config, &auth, get("/participant")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let html = body(response).await;
    assert!(html.contains("enctype=\"multipart/form-data\""));
    assert!(html.contains(&format!("href=\"/alpha/participant/{encoded_run}\">2026-01-02 03:04:06 JST</a>")));
    assert!(html.find("03:04:06 JST").unwrap() < html.find("03:04:05 JST").unwrap());
    assert!(!html.contains("03:04:07 JST"));
    assert!(!html.contains("03:04:08 JST"));
    let response = call(&config, &auth, get(&format!("/participant/{encoded_run}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body(response).await;
    assert!(html.contains("<h2>2026-01-02 03:04:06 JST</h2>"));
    assert!(html.contains("nested/完成 稿&#38;?#%.pdf"));
    assert!(html.contains("&#60;script&#62;.html"));
    for hidden in ["other.pdf", "discord-only.pdf", "similar.pdf", "full.pdf", "older.txt", "private.txt"] {
        assert!(!html.contains(hidden));
    }
    let encoded_key = utf8_percent_encode(filename, NON_ALPHANUMERIC);
    assert!(html.contains(&format!("/participant/{encoded_run}/file?key={encoded_key}")));
    for (suffix, disposition) in [("", "inline"), ("&download=true", "attachment"), ("&username=other&user_id=2", "inline")] {
        let response = call(&config, &auth, get(&format!("/participant/{encoded_run}/file?key={encoded_key}{suffix}"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "application/pdf");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        assert!(response.headers()["content-disposition"].to_str().unwrap().starts_with(disposition));
        assert_eq!(body(response).await, "%PDF-own");
    }
    let response = call(&config, &auth, get(&format!("/participant/{encoded_run}/file?key=%3Cscript%3E.html"))).await;
    assert_eq!(response.headers()["content-type"], "text/plain; charset=utf-8");
    assert!(response.headers()["content-security-policy"].to_str().unwrap().contains("sandbox"));
    for permissions in [UserPermissionSet::PARTICIPANT, UserPermissionSet::all()] {
        auth.permission_set = permissions;
        for key in [
            "other.pdf",
            "full.pdf",
            "authors/other/other.pdf",
            "similar.pdf",
            "older.txt",
            "private.txt",
        ] {
            assert_eq!(
                call(&config, &auth, get(&format!("/participant/{encoded_run}/file?key={key}")))
                    .await
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
    }
    for key in [
        "../other/other.pdf",
        "%2E%2E%2Fother%2Fother.pdf",
        "../../full.pdf",
        "/projects/2/results/file",
        "..%5Cother.pdf",
    ] {
        assert_eq!(
            call(&config, &auth, get(&format!("/participant/{encoded_run}/file?key={key}")))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for invalid_run in ["%2E%2E", "run%2Fauthors%2Fother", "run%5Cother", "run%00"] {
        for suffix in ["", "/file?key=other.pdf"] {
            assert_eq!(
                call(&config, &auth, get(&format!("/participant/{invalid_run}{suffix}"))).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
    discord_user::insert(
        &config.db(),
        TestDiscordUser {
            id: 2,
            discord_id: Some("67890".into()),
            username: "discord.other".into(),
        },
    )
    .await;
    auth.user_id = serde_json::from_str("2").unwrap();
    auth.username = "other".into();
    auth.permission_set = UserPermissionSet::PARTICIPANT;
    let response = call(&config, &auth, get(&format!("/participant/{encoded_run}/file?key=other.pdf"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, "other author");
    assert_eq!(
        call(&config, &auth, get(&format!("/participant/{encoded_run}/file?key={encoded_key}")))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let html = body(call(&config, &auth, get("/participant")).await).await;
    assert!(html.contains("03:04:07 JST"));
    assert!(!html.contains("03:04:05 JST"));
}

async fn participant_lists_all_own_results(env: Env) {
    let (config, auth) = setup(env, UserPermissionSet::PARTICIPANT).await;
    let bucket = config.bucket();
    let html = body(call(&config, &auth, get("/participant")).await).await;
    assert!(html.contains("完成稿はまだありません。"));
    for index in 0..101 {
        for key in [
            format!("projects/1/results/aaa/authors/other/{index:03}.txt"),
            format!("projects/1/results/{index:03}/authors/owner/result.txt"),
            format!("projects/1/results/run/authors/owner/{index:03}.txt"),
        ] {
            bucket.put(key, "result".to_owned()).execute().await.unwrap();
        }
    }
    let html = body(call(&config, &auth, get("/participant")).await).await;
    let mut previous = 0;
    for index in 0..101 {
        let link = format!("href=\"/alpha/participant/{index:03}\"");
        assert_eq!(html.matches(&link).count(), 1);
        let position = html.find(&link).unwrap();
        assert!(previous < position);
        previous = position;
    }
    assert_eq!(html.matches("href=\"/alpha/participant/run\"").count(), 1);
    assert!(!html.contains("href=\"/alpha/participant/aaa\""));
    let html = body(call(&config, &auth, get("/participant/run")).await).await;
    let mut previous = 0;
    for index in (0..101).rev() {
        let position = html.find(&format!("<li>{index:03}.txt")).unwrap();
        assert!(previous < position);
        previous = position;
    }
}

async fn participant_results_panic_on_invalid_username(env: Env) {
    let (config, mut auth) = setup(env, UserPermissionSet::PARTICIPANT).await;
    for username in ["", "..", "owner/../other", "owner\\other"] {
        auth.username = username.into();
        for path in ["/participant", "/participant/run", "/participant/run/file?key=test.pdf"] {
            AssertUnwindSafe(call(&config, &auth, get(path)))
                .catch_unwind()
                .await
                .expect_err("Invalid internal usernames must panic rather than return an HTTP error");
        }
    }
}
