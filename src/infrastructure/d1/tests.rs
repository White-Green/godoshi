use super::*;
use crate::core::project::ProjectName;
use crate::test_db::project::{TestProject, insert, rows};
use crate::worker_tests::TestContext;
use worker::Env;

pub async fn run(context: &mut TestContext) {
    context.run(metadata_schemas_are_project_specific_and_replaceable).await;
    context.run(test_find_project).await;
    context.run(test_environment_upload_keys).await;
}

async fn metadata_schemas_are_project_specific_and_replaceable(env: Env) {
    let store = D1Store(env.d1("TEST_DB").unwrap());
    let alpha = insert(&store.0, TestProject { id: 1, name: "alpha".into() }).await;
    let beta = insert(&store.0, TestProject { id: 2, name: "beta".into() }).await;
    let schema = serde_json::from_str(r#"{"fields":[{"key":"name","label":"Name","type":"string"}]}"#).unwrap();
    store.save_metadata_schema(alpha, schema).await.unwrap();
    assert_eq!(store.metadata_schema(alpha).await.unwrap().fields[0].key, "name");
    assert!(store.metadata_schema(beta).await.unwrap().fields.is_empty());
    let schema = serde_json::from_str(r#"{"fields":[{"key":"portrait","label":"Portrait","type":"file","accept":"image/*"}]}"#).unwrap();
    store.save_metadata_schema(alpha, schema).await.unwrap();
    assert_eq!(store.metadata_schema(alpha).await.unwrap().fields[0].key, "portrait");
}

async fn test_environment_upload_keys(env: Env) {
    let store = D1Store(env.d1("TEST_DB").unwrap());
    let alpha = insert(&store.0, TestProject { id: 1, name: "alpha".into() }).await;
    let beta = insert(&store.0, TestProject { id: 2, name: "beta".into() }).await;
    let (first, second) = futures::join!(
        store.create_environment_upload_key(alpha, "alpha-first"),
        store.create_environment_upload_key(alpha, "alpha-second")
    );
    first.unwrap();
    second.unwrap();
    let key = store.environment_upload_key(alpha).await.unwrap().unwrap();
    assert!(matches!(key.as_str(), "alpha-first" | "alpha-second"));
    let other_key = if key == "alpha-first" { "alpha-second" } else { "alpha-first" };
    assert_eq!(store.find_environment_upload_project(&key).await.unwrap(), Some(alpha));
    assert_eq!(store.find_environment_upload_project(other_key).await.unwrap(), None);
    store.create_environment_upload_key(beta, "beta-key").await.unwrap();
    store.delete_environment_upload_key(beta, &key).await.unwrap();
    assert_eq!(store.environment_upload_key(alpha).await.unwrap().as_deref(), Some(key.as_str()));
    assert_eq!(store.find_environment_upload_project("beta-key").await.unwrap(), Some(beta));
    store.delete_environment_upload_key(alpha, &key).await.unwrap();
    assert_eq!(store.environment_upload_key(alpha).await.unwrap(), None);
    assert_eq!(store.find_environment_upload_project(&key).await.unwrap(), None);
}

async fn test_find_project(env: Env) {
    let store = D1Store(env.d1("TEST_DB").unwrap());
    let db = &store.0;
    insert(db, TestProject { id: 1, name: "alpha".into() }).await;
    insert(db, TestProject { id: 2, name: "beta".into() }).await;
    let before = rows(db).await;

    assert_eq!(
        store.find_project("beta").await.unwrap(),
        Some(Project {
            id: ProjectId::new(2),
            name: ProjectName::new("beta".into()).unwrap()
        })
    );
    assert!(store.find_project("be").await.unwrap().is_none());
    assert!(store.find_project("Alpha").await.unwrap().is_none());
    assert_eq!(rows(db).await, before);
}
