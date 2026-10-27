use crate::app;
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{Bucket, ProjectBucket, ResultKey};
use crate::core::metadata::{AuthorMetadataBucket, MetadataChange, MetadataFile, MetadataUpdate};
use crate::core::operation_log::OperationLog;
use crate::core::project::ProjectStore;
use crate::core::user::{ProjectUserStore, UserId, UserPermissionSet, UserStore};
use crate::test_support::{TestConfig, discord_user, project, project_member, username};
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Router};
use time::{Duration, OffsetDateTime};

const SEED_TIMESTAMP: i64 = 1_793_044_800;
const SAMPLE_TEXT: &str = "合同誌のサンプル原稿\n\n画面の確認用に用意した固定データです。\n";

pub async fn router() -> Router {
    // Recreate the same seed for every request, so reloads and pagination are reproducible.
    let state = seed().await;
    let mut router = Router::new().route("/", axum::routing::get(async || Redirect::to("/demo/")));
    for name in ["demo", "empty"] {
        let project = state.projects().find_project(name).await.unwrap().unwrap();
        let user = state.users().project(project.id).find_member(UserId::new(1)).await.unwrap().unwrap();
        router = router.nest_service(
            &format!("/{name}"),
            app::route(state.clone())
                .layer(Extension(project.clone()))
                .layer(Extension(AuthState { project, user })),
        );
    }
    router.layer(axum::middleware::from_fn(read_only))
}

async fn read_only(request: Request, next: Next) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            "画面プレビューでは保存・アップロードはできません。前のページに戻ってください。",
        )
            .into_response();
    }
    next.run(request).await
}

async fn seed() -> TestConfig {
    let state = TestConfig::default().with_base_url("http://localhost:8787".into());
    let db = state.db();
    for (id, name) in [(1, "demo"), (2, "empty")] {
        project::insert(&db, project::TestProject { id, name: name.into() }).await;
        project_member::insert(
            &db,
            project_member::TestProjectMember {
                project_id: id,
                user_id: 1,
                username: "preview_editor".into(),
                permission_set: UserPermissionSet::all().bits().into(),
            },
        )
        .await;
    }
    for (id, name, permissions) in [
        (1, "preview_editor", UserPermissionSet::all()),
        (2, "sora", UserPermissionSet::PARTICIPANT),
        (3, "reviewer", UserPermissionSet::REVIEWER),
        (
            4,
            "long_name_for_layout_preview",
            UserPermissionSet::PARTICIPANT | UserPermissionSet::REVIEWER,
        ),
        (5, "new_member", UserPermissionSet::empty()),
    ] {
        discord_user::insert(
            &db,
            discord_user::TestDiscordUser {
                id,
                discord_id: None,
                username: name.into(),
            },
        )
        .await;
        project_member::insert(
            &db,
            project_member::TestProjectMember {
                project_id: 1,
                user_id: id,
                username: name.into(),
                permission_set: permissions.bits().into(),
            },
        )
        .await;
    }

    let project = state.projects().find_project("demo").await.unwrap().unwrap();
    state
        .projects()
        .save_metadata_schema(
            project.id,
            serde_json::from_str(
                r#"{"fields":[
                    {"key":"display_name","label":"表記名","description":"誌面に掲載する名前を入力してください。","type":"string","max_length":100},
                    {"key":"title","label":"作品タイトル","type":"string","max_length":200},
                    {"key":"afterword","label":"あとがき","description":"改行を含む文章の表示も確認できます。","type":"string","multiline":true,"max_length":2000},
                    {"key":"profile","label":"プロフィール資料","description":"提出済みファイルの表示例です。","type":"file","accept":".txt,.pdf"},
                    {"key":"portrait","label":"プロフィール画像","description":"未提出のファイル項目です。","type":"file","accept":"image/*"}
                ]}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    state
        .projects()
        .create_environment_upload_key(project.id, "preview-environment")
        .await
        .unwrap();
    let bucket = state.bucket();
    bucket
        .project(project.id)
        .metadata_by_user(UserId::new(1))
        .save(MetadataUpdate(
            [
                ("display_name".into(), MetadataChange::String("サンプル編集者".into())),
                ("title".into(), MetadataChange::String("小さな物語を集めて — 合同誌制作の記録".into())),
                (
                    "afterword".into(),
                    MetadataChange::String("参加してくださった皆さん、ありがとうございます。\nまた次の一冊でお会いしましょう。".into()),
                ),
                (
                    "profile".into(),
                    MetadataChange::File(MetadataFile::new(SAMPLE_TEXT.as_bytes().into(), "profile.txt".into()).unwrap()),
                ),
            ]
            .into(),
        ))
        .await
        .unwrap();

    let timestamp = OffsetDateTime::from_unix_timestamp(SEED_TIMESTAMP).unwrap();
    for index in 0..3 {
        let run = ResultKey::for_build(timestamp - Duration::days(index), index as u64 + 1);
        for file in [
            "完成稿.txt",
            "authors/preview_editor/原稿.txt",
            "authors/preview_editor/長いファイル名の折り返しを確認するためのサンプル原稿_第二版_校正済み.txt",
            "authors/sora/原稿.txt",
        ] {
            bucket
                .put(format!("projects/{}/results/{}/{file}", project.id, run.as_str()), SAMPLE_TEXT)
                .await
                .unwrap();
        }
    }
    for index in 0..55 {
        let time = timestamp - Duration::minutes(index * 15);
        let run_id = ResultKey::for_build(time, index as u64 + 1);
        let event = match index % 5 {
            0 => OperationLog::BuildSucceeded { run_id },
            1 => OperationLog::BuildStarted { run_id },
            2 => OperationLog::ManuscriptUpdated {
                user_id: UserId::new(2),
                username: username("sora"),
                filename: "1.pdf".into(),
            },
            3 => OperationLog::BuildFailed { run_id, exit_code: Some(1) },
            _ => OperationLog::EnvironmentUpdated,
        };
        bucket.seed_log(project.id, time, event);
    }
    state
}
