use crate::core::project::ProjectId;
use crate::core::user::discord::DiscordUserStore;
use crate::core::user::{ProjectMember, ProjectUserStore, UserId, UserPermissionSet, UserStore, Username};
use crate::infrastructure::d1::D1Store;
use crate::test_db::discord_user::TestDiscordUser;
use crate::test_db::project::TestProject;
use crate::test_db::project_member::TestProjectMember;
use crate::test_db::{discord_user, project, project_member};
use crate::worker_tests::TestContext;
use worker::Env;

pub async fn run(context: &mut TestContext) {
    context.run(test_user).await;
    context.run(test_sync_discord_user).await;
    context.run(test_init_member).await;
}

async fn test_user(env: Env) {
    let store = D1Store(env.d1("TEST_DB").unwrap());
    project::insert(&store.0, TestProject { id: 1, name: "alpha".into() }).await;
    project::insert(&store.0, TestProject { id: 2, name: "beta".into() }).await;
    for (id, username) in [(1, "author"), (2, "other"), (3, "third")] {
        crate::test_db::discord_user::insert(
            &store.0,
            TestDiscordUser {
                id,
                discord_id: None,
                username: username.to_owned(),
            },
        )
        .await;
    }
    for (project_id, user_id, username) in [(1, 1, "author"), (1, 2, "other"), (2, 2, "author")] {
        crate::test_db::project_member::insert(
            &store.0,
            crate::test_db::project_member::TestProjectMember {
                project_id,
                user_id,
                username: username.to_owned(),
                permission_set: 4,
            },
        )
        .await;
    }
    let mut before_project_member = crate::test_db::project_member::rows(&store.0).await;
    let alpha = store.project(ProjectId::new(1));
    let beta = store.project(ProjectId::new(2));

    assert_eq!(
        alpha.all_member().await.unwrap(),
        [
            ProjectMember::new(UserId::new(1), Username::new("author".into()).unwrap(), UserPermissionSet::PARTICIPANT),
            ProjectMember::new(UserId::new(2), Username::new("other".into()).unwrap(), UserPermissionSet::PARTICIPANT),
        ]
    );
    assert_eq!(
        beta.all_member().await.unwrap(),
        [ProjectMember::new(
            UserId::new(2),
            Username::new("author".into()).unwrap(),
            UserPermissionSet::PARTICIPANT,
        )]
    );

    assert_eq!(
        alpha.find_member(UserId::new(1)).await.unwrap().unwrap(),
        ProjectMember::new(UserId::new(1), Username::new("author".into()).unwrap(), UserPermissionSet::PARTICIPANT)
    );
    assert_eq!(
        alpha.find_member(UserId::new(2)).await.unwrap().unwrap(),
        ProjectMember::new(UserId::new(2), Username::new("other".into()).unwrap(), UserPermissionSet::PARTICIPANT)
    );
    assert!(alpha.find_member(UserId::new(3)).await.unwrap().is_none());
    assert!(beta.find_member(UserId::new(1)).await.unwrap().is_none());
    assert_eq!(
        beta.find_member(UserId::new(2)).await.unwrap().unwrap(),
        ProjectMember::new(UserId::new(2), Username::new("author".into()).unwrap(), UserPermissionSet::PARTICIPANT)
    );
    assert!(beta.find_member(UserId::new(3)).await.unwrap().is_none());

    alpha
        .update_permission(UserId::new(1), UserPermissionSet::ADMIN | UserPermissionSet::REVIEWER)
        .await
        .unwrap();
    assert_eq!(
        alpha.find_member(UserId::new(1)).await.unwrap().unwrap(),
        ProjectMember::new(
            UserId::new(1),
            Username::new("author".into()).unwrap(),
            UserPermissionSet::ADMIN | UserPermissionSet::REVIEWER
        )
    );

    before_project_member[0].permission_set = 3;

    assert_eq!(crate::test_db::project_member::rows(&store.0).await, before_project_member);
}

async fn test_sync_discord_user(env: Env) {
    let store = D1Store(env.d1("TEST_DB").unwrap());
    project::insert(&store.0, TestProject { id: 1, name: "alpha".into() }).await;
    for (id, username) in [(1, "author"), (2, "other")] {
        discord_user::insert(
            &store.0,
            TestDiscordUser {
                id,
                discord_id: None,
                username: username.to_owned(),
            },
        )
        .await;
    }
    for (project_id, user_id, username) in [(1, 1, "author")] {
        project_member::insert(
            &store.0,
            TestProjectMember {
                project_id,
                user_id,
                username: username.to_owned(),
                permission_set: 4,
            },
        )
        .await;
    }
    let alpha = store.project(ProjectId::new(1));
    alpha.sync_discord_user("12345678901234567".into(), "author".into()).await.unwrap();
    assert_eq!(
        discord_user::rows(&store.0).await,
        [
            TestDiscordUser {
                id: 1,
                discord_id: Some("12345678901234567".into()),
                username: "author".into(),
            },
            TestDiscordUser {
                id: 2,
                discord_id: None,
                username: "other".into(),
            },
        ]
    );
    assert_eq!(
        project_member::rows(&store.0).await,
        [TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "author".into(),
            permission_set: 4,
        },]
    );
    assert_eq!(
        alpha
            .sync_discord_user("12345678901234567".into(), "author2".into())
            .await
            .unwrap()
            .unwrap(),
        ProjectMember::new(UserId::new(1), Username::new("author".into()).unwrap(), UserPermissionSet::PARTICIPANT)
    );
    assert_eq!(
        discord_user::rows(&store.0).await,
        [
            TestDiscordUser {
                id: 1,
                discord_id: Some("12345678901234567".into()),
                username: "author2".into(),
            },
            TestDiscordUser {
                id: 2,
                discord_id: None,
                username: "other".into(),
            },
        ]
    );
    assert_eq!(
        project_member::rows(&store.0).await,
        [TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "author".into(),
            permission_set: 4,
        },]
    );
    assert!(
        alpha
            .sync_discord_user("12345678901234560".into(), "author2".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        alpha
            .sync_discord_user("12345678901234560".into(), "author3".into())
            .await
            .unwrap()
            .is_none()
    );
}

async fn test_init_member(env: Env) {
    let store = D1Store(env.d1("TEST_DB").unwrap());
    project::insert(&store.0, TestProject { id: 1, name: "alpha".into() }).await;
    for (id, username) in [(1, "author")] {
        discord_user::insert(
            &store.0,
            TestDiscordUser {
                id,
                discord_id: Some("12345678901234567".into()),
                username: username.to_owned(),
            },
        )
        .await;
    }
    let alpha = store.project(ProjectId::new(1));
    assert_eq!(
        alpha
            .init_member(Username::new("author".into()).unwrap(), UserPermissionSet::ADMIN)
            .await
            .unwrap(),
        ProjectMember::new(UserId::new(1), Username::new("author".into()).unwrap(), UserPermissionSet::ADMIN)
    );
    assert_eq!(
        discord_user::rows(&store.0).await,
        [TestDiscordUser {
            id: 1,
            discord_id: Some("12345678901234567".into()),
            username: "author".into(),
        },]
    );
    assert_eq!(
        project_member::rows(&store.0).await,
        [TestProjectMember {
            project_id: 1,
            user_id: 1,
            username: "author".into(),
            permission_set: 1,
        },]
    );

    assert_eq!(
        alpha
            .init_member(Username::new("author2".into()).unwrap(), UserPermissionSet::ADMIN)
            .await
            .unwrap(),
        ProjectMember::new(UserId::new(2), Username::new("author2".into()).unwrap(), UserPermissionSet::ADMIN)
    );
    assert_eq!(
        discord_user::rows(&store.0).await,
        [
            TestDiscordUser {
                id: 1,
                discord_id: Some("12345678901234567".into()),
                username: "author".into(),
            },
            TestDiscordUser {
                id: 2,
                discord_id: None,
                username: "author2".into(),
            },
        ]
    );
    assert_eq!(
        project_member::rows(&store.0).await,
        [
            TestProjectMember {
                project_id: 1,
                user_id: 1,
                username: "author".into(),
                permission_set: 1,
            },
            TestProjectMember {
                project_id: 1,
                user_id: 2,
                username: "author2".into(),
                permission_set: 1,
            },
        ]
    );
}
