use super::*;
use crate::core::project::ProjectName;
use crate::test_db::project::{TestProject, insert, rows};
use crate::worker_tests::TestContext;
use worker::Env;

pub async fn run(context: &mut TestContext) {
    context.run(test_find_project).await;
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
