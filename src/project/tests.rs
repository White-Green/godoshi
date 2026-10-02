use super::{ProjectName, find};
use crate::project::ProjectId;
use crate::test_db::project::{TestProject, insert, rows};
use crate::worker_tests::TestContext;
use worker::Env;

pub async fn run(context: &mut TestContext) {
    context.run(missing_project).await;
    context.run(existing_project).await;
    context.run(exact_project_name).await;
    context.run(case_sensitive_project_name).await;
}

async fn missing_project(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    let name = ProjectName::new("missing".into()).unwrap();

    assert_eq!(find(&db, &name).await.unwrap(), None);
    assert!(rows(&db).await.is_empty());
}

async fn existing_project(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(&db, TestProject { id: 1, name: "alpha".into() }).await;
    insert(&db, TestProject { id: 2, name: "beta".into() }).await;
    let before = rows(&db).await;
    let name = ProjectName::new("beta".into()).unwrap();

    assert_eq!(find(&db, &name).await.unwrap(), Some(ProjectId(2)));
    assert_eq!(rows(&db).await, before);
}

async fn exact_project_name(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(
        &db,
        TestProject {
            id: 1,
            name: "alphaX1-test".into(),
        },
    )
    .await;
    insert(
        &db,
        TestProject {
            id: 2,
            name: "alpha_1-test".into(),
        },
    )
    .await;
    let before = rows(&db).await;
    let exact = ProjectName::new("alpha_1-test".into()).unwrap();
    let prefix = ProjectName::new("alpha_1".into()).unwrap();

    assert_eq!(find(&db, &exact).await.unwrap(), Some(ProjectId(2)));
    assert_eq!(find(&db, &prefix).await.unwrap(), None);
    assert_eq!(rows(&db).await, before);
}

async fn case_sensitive_project_name(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    insert(&db, TestProject { id: 1, name: "Alpha".into() }).await;
    let before = rows(&db).await;
    let exact = ProjectName::new("Alpha".into()).unwrap();
    let lowercase = ProjectName::new("alpha".into()).unwrap();

    assert_eq!(find(&db, &exact).await.unwrap(), Some(ProjectId(1)));
    assert_eq!(find(&db, &lowercase).await.unwrap(), None);
    assert_eq!(rows(&db).await, before);
}
