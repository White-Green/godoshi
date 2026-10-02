use super::{DiscordUser, find};
use crate::test_db::discord_user::{TestDiscordUser, insert, rows};
use crate::user::UserId;
use crate::worker_tests::TestContext;
use smol_str::SmolStr;
use worker::Env;

// Snowflake-sized identifiers whose final digits must remain distinct.
const DISCORD_USER_ID: SmolStr = SmolStr::new_inline("1234567890123456789");
const OTHER_DISCORD_USER_ID: SmolStr = SmolStr::new_inline("1234567890123456790");

pub async fn run(context: &mut TestContext) {
    context.run(missing_user).await;
    context.run(existing_user).await;
    context.run(distinct_discord_ids).await;
    context.run(renamed_user).await;
    context.run(link_unclaimed_user).await;
    context.run(username_belongs_to_another_user).await;
    context.run(ambiguous_users).await;
}

async fn missing_user(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    assert!(find(&db, DISCORD_USER_ID, "alice".into()).await.unwrap().is_none());
    assert!(rows(&db).await.is_empty());
}

async fn existing_user(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: Some(DISCORD_USER_ID),
            username: "alice".into(),
        },
    )
    .await;
    let before = rows(&db).await;

    let user = find(&db, DISCORD_USER_ID, "alice".into())
        .await
        .unwrap()
        .expect("existing user should be found");

    assert_eq!(
        user,
        DiscordUser {
            id: UserId(1),
            discord_user_id: DISCORD_USER_ID,
            discord_username: "alice".into(),
        },
    );
    assert_eq!(rows(&db).await, before);
}

async fn distinct_discord_ids(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: Some(DISCORD_USER_ID),
            username: "alice".into(),
        },
    )
    .await;
    insert(
        &db,
        TestDiscordUser {
            id: 2,
            discord_id: Some(OTHER_DISCORD_USER_ID),
            username: "bob".into(),
        },
    )
    .await;

    assert_eq!(
        rows(&db).await,
        vec![
            TestDiscordUser {
                id: 1,
                discord_id: Some(DISCORD_USER_ID),
                username: "alice".to_owned(),
            },
            TestDiscordUser {
                id: 2,
                discord_id: Some(OTHER_DISCORD_USER_ID),
                username: "bob".to_owned(),
            },
        ],
    );

    let alice = find(&db, DISCORD_USER_ID, "alice".into())
        .await
        .unwrap()
        .expect("first Discord ID should be found");
    let bob = find(&db, OTHER_DISCORD_USER_ID, "bob".into())
        .await
        .unwrap()
        .expect("second Discord ID should be found");
    assert_eq!(
        alice,
        DiscordUser {
            id: UserId(1),
            discord_user_id: DISCORD_USER_ID,
            discord_username: "alice".into(),
        },
    );
    assert_eq!(
        bob,
        DiscordUser {
            id: UserId(2),
            discord_user_id: OTHER_DISCORD_USER_ID,
            discord_username: "bob".into(),
        },
    );
}

async fn renamed_user(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: Some(DISCORD_USER_ID),
            username: "alice_old".into(),
        },
    )
    .await;

    let user = find(&db, DISCORD_USER_ID, "alice_new".into())
        .await
        .unwrap()
        .expect("renamed user should be found by Discord ID");

    assert_eq!(
        rows(&db).await,
        vec![TestDiscordUser {
            id: 1,
            discord_id: Some(DISCORD_USER_ID),
            username: "alice_new".to_owned()
        }],
        "the stored username should be updated",
    );
    assert_eq!(
        user,
        DiscordUser {
            id: UserId(1),
            discord_user_id: DISCORD_USER_ID,
            discord_username: "alice_new".into(),
        },
    );
}

async fn link_unclaimed_user(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: None,
            username: "alice".into(),
        },
    )
    .await;
    insert(
        &db,
        TestDiscordUser {
            id: 2,
            discord_id: None,
            username: "bob".into(),
        },
    )
    .await;

    let user = find(&db, DISCORD_USER_ID, "alice".into())
        .await
        .unwrap()
        .expect("unclaimed username should be linked");

    assert_eq!(
        user,
        DiscordUser {
            id: UserId(1),
            discord_user_id: DISCORD_USER_ID,
            discord_username: "alice".into(),
        },
    );
    assert_eq!(
        rows(&db).await,
        vec![
            TestDiscordUser {
                id: 1,
                discord_id: Some(DISCORD_USER_ID),
                username: "alice".to_owned()
            },
            TestDiscordUser {
                id: 2,
                discord_id: None,
                username: "bob".to_owned()
            }
        ]
    );
}

async fn username_belongs_to_another_user(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: Some(OTHER_DISCORD_USER_ID),
            username: "alice".into(),
        },
    )
    .await;
    let before = rows(&db).await;

    assert!(find(&db, DISCORD_USER_ID, "alice".into()).await.unwrap().is_none());
    assert_eq!(rows(&db).await, before);
}

async fn ambiguous_users(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestDiscordUser {
            id: 1,
            discord_id: Some(DISCORD_USER_ID),
            username: "alice_old".into(),
        },
    )
    .await;
    insert(
        &db,
        TestDiscordUser {
            id: 2,
            discord_id: None,
            username: "alice_new".into(),
        },
    )
    .await;
    let before = rows(&db).await;

    assert!(find(&db, DISCORD_USER_ID, "alice_new".into()).await.unwrap().is_none());
    assert_eq!(rows(&db).await, before);
}
