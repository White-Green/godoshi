use super::{AuthState, construct_auth_state};
use crate::config::GenericConfigProvider;
use crate::project::{ProjectId, ProjectName, ProjectState};
use crate::test_db::permission::TestPermission;
use crate::test_db::project::TestProject;
use crate::test_db::user::TestUser;
use crate::test_db::{permission, project, user};
use crate::user::UserPermissionSet;
use crate::worker_tests::TestContext;
use worker::{D1Database, Env};

pub async fn run(context: &mut TestContext) {
    context.run(missing_permissions).await;
    context.run(empty_permissions).await;
    context.run(admin_permission).await;
    context.run(participant_permission).await;
    context.run(combined_permissions).await;
    context.run(permissions_are_scoped_to_user).await;
    context.run(permissions_are_scoped_to_project).await;
}

struct TestConfig(Env);

impl GenericConfigProvider for TestConfig {
    fn base_url(&self) -> String {
        "http://localhost".to_owned()
    }

    fn db(&self) -> D1Database {
        self.0.d1("TEST_DB").unwrap()
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
        permission::insert(
            &db,
            TestPermission {
                project_id: 1,
                user_id: 1,
                permission_set: permissions.bits().into(),
            },
        )
        .await;
    }
    let before = permission::rows(&db).await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await;

    assert_eq!(
        auth,
        AuthState {
            user_id,
            project,
            permission_set: permissions.unwrap_or_else(UserPermissionSet::empty),
        },
    );
    assert_eq!(permission::rows(&db).await, before);
}

async fn missing_permissions(env: Env) {
    check_permission_set(env, None).await;
}

async fn empty_permissions(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::empty())).await;
}

async fn admin_permission(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::ADMIN)).await;
}

async fn participant_permission(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::PERTICIPENT)).await;
}

async fn combined_permissions(env: Env) {
    check_permission_set(env, Some(UserPermissionSet::ADMIN | UserPermissionSet::PERTICIPENT)).await;
}

async fn permissions_are_scoped_to_user(env: Env) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    let other_user_id = user::insert(&db, TestUser { id: 2 }).await;
    permission::insert(
        &db,
        TestPermission {
            project_id: 1,
            user_id: 2,
            permission_set: UserPermissionSet::ADMIN.bits().into(),
        },
    )
    .await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await;
    assert_eq!(
        auth,
        AuthState {
            user_id,
            project: project.clone(),
            permission_set: UserPermissionSet::empty(),
        },
    );

    permission::insert(
        &db,
        TestPermission {
            project_id: 1,
            user_id: 1,
            permission_set: UserPermissionSet::PERTICIPENT.bits().into(),
        },
    )
    .await;
    let before = permission::rows(&db).await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await;
    let other_auth = construct_auth_state(&config, other_user_id, project.clone()).await;

    assert_eq!(
        auth,
        AuthState {
            user_id,
            project: project.clone(),
            permission_set: UserPermissionSet::PERTICIPENT,
        },
    );
    assert_eq!(
        other_auth,
        AuthState {
            user_id: other_user_id,
            project,
            permission_set: UserPermissionSet::ADMIN,
        },
    );
    assert_eq!(permission::rows(&db).await, before);
}

async fn permissions_are_scoped_to_project(env: Env) {
    let config = TestConfig(env);
    let db = config.db();
    let project = project_state(project::insert(&db, TestProject { id: 1, name: "alpha".into() }).await, "alpha");
    let other_project = project_state(project::insert(&db, TestProject { id: 2, name: "beta".into() }).await, "beta");
    let user_id = user::insert(&db, TestUser { id: 1 }).await;
    permission::insert(
        &db,
        TestPermission {
            project_id: 2,
            user_id: 1,
            permission_set: UserPermissionSet::ADMIN.bits().into(),
        },
    )
    .await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await;
    assert_eq!(
        auth,
        AuthState {
            user_id,
            project: project.clone(),
            permission_set: UserPermissionSet::empty(),
        },
    );

    permission::insert(
        &db,
        TestPermission {
            project_id: 1,
            user_id: 1,
            permission_set: UserPermissionSet::PERTICIPENT.bits().into(),
        },
    )
    .await;
    let before = permission::rows(&db).await;

    let auth = construct_auth_state(&config, user_id, project.clone()).await;
    let other_auth = construct_auth_state(&config, user_id, other_project.clone()).await;

    assert_eq!(
        auth,
        AuthState {
            user_id,
            project,
            permission_set: UserPermissionSet::PERTICIPENT,
        },
    );
    assert_eq!(
        other_auth,
        AuthState {
            user_id,
            project: other_project,
            permission_set: UserPermissionSet::ADMIN,
        },
    );
    assert_eq!(permission::rows(&db).await, before);
}
