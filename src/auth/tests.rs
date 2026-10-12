use super::{AuthState, construct_auth_state};
use crate::config::GenericConfigProvider;
use crate::project::{ProjectId, ProjectName, ProjectState};
use crate::test_db::project::TestProject;
use crate::test_db::project_member::TestProjectMember;
use crate::test_db::user::TestUser;
use crate::test_db::{project, project_member, user};
use crate::user::UserPermissionSet;
use crate::worker_tests::TestContext;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use worker::{Bucket, D1Database, Env};

pub async fn run(context: &mut TestContext) {
    context.run(missing_permissions).await;
    context.run(empty_permissions).await;
    context.run(admin_permission).await;
    context.run(participant_permission).await;
    context.run(combined_permissions).await;
    context.run(permissions_are_scoped_to_user).await;
    context.run(permissions_are_scoped_to_project).await;
    context.run(invalid_permission_result_panics).await;
    context.run(username_is_independent_of_discord).await;
}

struct TestConfig(Env);

impl GenericConfigProvider for TestConfig {
    fn base_url(&self) -> String {
        "http://localhost".to_owned()
    }

    fn db(&self) -> D1Database {
        self.0.d1("TEST_DB").unwrap()
    }

    fn bucket(&self) -> Bucket {
        self.0.bucket("TEST_BUCKET").unwrap()
    }
}

fn project_state(id: ProjectId, name: &str) -> ProjectState {
    ProjectState {
        id,
        name: ProjectName::new(name.into()).unwrap(),
    }
}

async fn check_permission_set(env: Env, permissions: Option<UserPermissionSet>) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    if let Some(permissions) = permissions {
        project_member::insert(
            &db,
            TestProjectMember {
                project_id: 1,
                user_id: 1,
                username: "alice".into(),
                permission_set: permissions.bits().into(),
            },
        )
        .await;
    }
    let before = project_member::rows(&db).await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await.unwrap();

    assert_eq!(
        auth,
        AuthState {
            user_id,
            project,
            username: permissions.map(|_| "alice".into()),
            permission_set: permissions.unwrap_or_else(UserPermissionSet::empty),
        },
    );
    assert_eq!(project_member::rows(&db).await, before);
}

async fn missing_permissions(env: Env) {
    check_permission_set(env, None).await;
}

async fn invalid_permission_result_panics(env: Env) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "alice".into(),
            permission_set: 0b1000,
        },
    )
    .await;

    // The query succeeds, but its data violates the application's permission type.
    let panic = AssertUnwindSafe(construct_auth_state(&config, user_id, project))
        .catch_unwind()
        .await
        .expect_err("Invalid internal data must panic rather than return an HTTP error");
    let message = panic.downcast_ref::<String>().expect("expect must include its diagnostic message");
    assert!(message.contains("Project member query results must match the schema and permission bits"));
}

async fn empty_permissions(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::empty())).await;
}

async fn admin_permission(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::ADMIN)).await;
}

async fn participant_permission(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::PARTICIPANT)).await;
}

async fn combined_permissions(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::ADMIN | UserPermissionSet::PARTICIPANT)).await;
}

async fn permissions_are_scoped_to_user(env: Env) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    let other_user_id = user::insert(&db, TestUser { id: 2 }).await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 2,
            username: "bob".into(),
            permission_set: UserPermissionSet::ADMIN.bits().into(),
        },
    )
    .await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await.unwrap();
    assert_eq!(
        auth,
        AuthState {
            user_id,
            project: project.clone(),
            username: None,
            permission_set: UserPermissionSet::empty(),
        },
    );

    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "alice".into(),
            permission_set: UserPermissionSet::PARTICIPANT.bits().into(),
        },
    )
    .await;
    let before = project_member::rows(&db).await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await.unwrap();
    let other_auth = construct_auth_state(&config, other_user_id, project.clone()).await.unwrap();

    assert_eq!(
        auth,
        AuthState {
            user_id,
            project: project.clone(),
            username: Some("alice".into()),
            permission_set: UserPermissionSet::PARTICIPANT,
        },
    );
    assert_eq!(
        other_auth,
        AuthState {
            user_id: other_user_id,
            project,
            username: Some("bob".into()),
            permission_set: UserPermissionSet::ADMIN,
        },
    );
    assert_eq!(project_member::rows(&db).await, before);
}

async fn permissions_are_scoped_to_project(env: Env) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let other_project = project_state(project::insert(&db, TestProject { id: 2, name: "beta".into() }).await, "beta");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 2,
            user_id: 1,
            username: "beta-author".into(),
            permission_set: UserPermissionSet::ADMIN.bits().into(),
        },
    )
    .await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await.unwrap();
    assert_eq!(
        auth,
        AuthState {
            user_id,
            project: project.clone(),
            username: None,
            permission_set: UserPermissionSet::empty(),
        },
    );

    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "alpha-author".into(),
            permission_set: UserPermissionSet::PARTICIPANT.bits().into(),
        },
    )
    .await;
    let before = project_member::rows(&db).await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await.unwrap();
    let other_auth = construct_auth_state(&config, user_id, other_project.clone()).await.unwrap();

    assert_eq!(
        auth,
        AuthState {
            user_id,
            project,
            username: Some("alpha-author".into()),
            permission_set: UserPermissionSet::PARTICIPANT,
        },
    );
    assert_eq!(
        other_auth,
        AuthState {
            user_id,
            project: other_project,
            username: Some("beta-author".into()),
            permission_set: UserPermissionSet::ADMIN,
        },
    );
    assert_eq!(project_member::rows(&db).await, before);
}

async fn username_is_independent_of_discord(env: Env) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    db.prepare("INSERT INTO discord_user (id, discord_id, username) VALUES (1, '12345', 'discord.before')")
        .run()
        .await
        .unwrap();
    project_member::insert(
        &db,
        TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "pen.name".into(),
            permission_set: UserPermissionSet::PARTICIPANT.bits().into(),
        },
    )
    .await;
    let before = construct_auth_state(&config, user_id, project.clone()).await.unwrap();
    assert_eq!(before.username.as_deref(), Some("pen.name"));
    let discord = crate::user::discord::find(&db, "12345".into(), "discord.after".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(discord.discord_username, "discord.after");
    assert_eq!(construct_auth_state(&config, user_id, project.clone()).await.unwrap(), before);
    db.prepare("UPDATE project_member SET username = 'new.pen.name' WHERE project_id = 1 AND user_id = 1")
        .run()
        .await
        .unwrap();
    let after = construct_auth_state(&config, user_id, project).await.unwrap();
    assert_eq!(after.username.as_deref(), Some("new.pen.name"));
    assert_eq!(after.permission_set, before.permission_set);
}
