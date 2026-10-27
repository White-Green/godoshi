use super::*;
use crate::test_db::resource_file_list::ResourceFileList;
use crate::test_db::{discord_user, project, project_member, resource_file_list};
use crate::worker_tests::TestContext;
use futures::stream::BoxStream;
use time::OffsetDateTime;
use worker::Env;

pub async fn run(context: &mut TestContext) {
    context.run(metadata_is_scoped_and_published_without_manuscripts).await;
    context.run(lists_are_scoped_to_members).await;
    context.run(resource_lists_reject_invalid_extensions).await;
    context.run(resource_lists_follow_user_ids).await;
    context.run(resource_updates_reject_username_reassignment).await;
    context.run(resources_are_scoped_and_streamed).await;
    context.run(user_result_list_requires_visible_files).await;
    context.run(results_paginate_and_isolate_authors).await;
    context.run(invalid_keys_do_not_escape_scopes).await;
    context.run(generated_keys_preserve_build_time).await;
    context.run(resource_lists_insert_remove_and_reorder).await;
    context.run(failed_resource_updates_preserve_existing_files).await;
    context.run(stream_uploads_preserve_other_objects_and_failed_replacements).await;
    context.run(stream_uploads_select_single_put_or_larger_parts).await;
}

async fn metadata_is_scoped_and_published_without_manuscripts(env: Env) {
    use crate::core::metadata::*;
    use crate::core::user::UserId;
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let metadata = project.metadata_by_user(UserId::new(1));
    assert!(project.resource().download("authors/author/metadata.json").await.unwrap().is_none());
    let update = MetadataUpdate(std::collections::BTreeMap::from([
        ("name".into(), MetadataChange::String("白緑".into())),
        (
            "icon".into(),
            MetadataChange::File(MetadataFile::new(Bytes::from_static(b"opaque bytes, not an image"), "original.png".into()).unwrap()),
        ),
    ]));
    metadata.save(update).await.unwrap();
    assert_eq!(metadata.load().await.unwrap().values["name"], "白緑");
    assert!(
        bucket
            .project(ProjectId::new(2))
            .metadata_by_user(UserId::new(1))
            .load()
            .await
            .unwrap()
            .values
            .is_empty()
    );
    assert!(project.metadata_by_user(UserId::new(2)).load().await.unwrap().values.is_empty());
    assert!(project.metadata_by_user(UserId::new(999)).load().await.is_err());

    let resource = project.resource();
    let document = text(resource.download("authors/author/metadata.json").await.unwrap().unwrap()).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&document).unwrap(),
        serde_json::json!({"icon":"assets/icon.png", "name":"白緑"})
    );
    let key = "authors/author/metadata.json";

    // R2 overwrites are independent of D1 writes: there is no rollback on a D1 failure.
    let db = env.d1("TEST_DB").unwrap();
    db.prepare("CREATE TRIGGER reject_metadata BEFORE UPDATE ON author_metadata BEGIN SELECT RAISE(ABORT, 'test failure'); END")
        .run()
        .await
        .unwrap();
    let rejected = metadata
        .save(MetadataUpdate(std::collections::BTreeMap::from([
            ("name".into(), MetadataChange::String("not published".into())),
            (
                "icon".into(),
                MetadataChange::File(MetadataFile::new(Bytes::from_static(b"overwritten"), "another.png".into()).unwrap()),
            ),
        ])))
        .await;
    assert!(rejected.is_err());
    assert_eq!(text(resource.download(key).await.unwrap().unwrap()).await, document);
    assert_eq!(text(metadata.file("icon").await.unwrap().unwrap().1).await, "overwritten");
    db.prepare("DROP TRIGGER reject_metadata").run().await.unwrap();

    // Basenames never affect the public name; even an extension change reuses the same R2 key.
    for (filename, published, bytes) in [("new-name.png", "icon.png", "second"), ("portrait.jpg", "icon.jpg", "last")] {
        metadata
            .save(MetadataUpdate(std::collections::BTreeMap::from([(
                "icon".into(),
                MetadataChange::File(MetadataFile::new(Bytes::copy_from_slice(bytes.as_bytes()), filename.into()).unwrap()),
            )])))
            .await
            .unwrap();
        assert_eq!(metadata.load().await.unwrap().files["icon"], published);
        assert_eq!(
            text(resource.download(&format!("authors/author/assets/{published}")).await.unwrap().unwrap()).await,
            bytes
        );
        let objects = bucket
            .bucket
            .list()
            .prefix("projects/1/metadata-assets/users/1/")
            .execute()
            .await
            .unwrap();
        assert_eq!(objects.objects().len(), 1);
        assert_eq!(objects.objects()[0].key(), "projects/1/metadata-assets/users/1/icon");
    }
    assert!(resource.download("authors/author/assets/icon.png").await.unwrap().is_none());

    // Changes are partial patches, and unrelated data survives manuscript replacement.
    metadata
        .save(MetadataUpdate(std::collections::BTreeMap::from([(
            "name".into(),
            MetadataChange::String("updated".into()),
        )])))
        .await
        .unwrap();
    assert!(metadata.load().await.unwrap().files.contains_key("icon"));
    project
        .resource_by_user(user("author"))
        .update(vec![file(data(b"draft"), Some("pdf"))])
        .await
        .unwrap();
    assert_eq!(metadata.load().await.unwrap().values["name"], "updated");
    metadata
        .save(MetadataUpdate(std::collections::BTreeMap::from([(
            "icon".into(),
            MetadataChange::Remove,
        )])))
        .await
        .unwrap();
    assert!(resource.download("authors/author/assets/icon.jpg").await.unwrap().is_none());
    assert!(bucket.bucket.head("projects/1/metadata-assets/users/1/icon").await.unwrap().is_some());
    assert!(resource.download("authors/author/1.pdf").await.unwrap().is_some());

    db.prepare("UPDATE project_member SET username = 'renamed' WHERE project_id = 1 AND user_id = 1")
        .run()
        .await
        .unwrap();
    assert!(resource.download(key).await.unwrap().is_none());
    assert!(resource.download("authors/renamed/metadata.json").await.unwrap().is_some());
}

fn data(bytes: &[u8]) -> BoxStream<'_, Result<Bytes, std::io::Error>> {
    futures::stream::iter(bytes.chunks(64 * 1024).map(|chunk| Ok(Bytes::copy_from_slice(chunk)))).boxed()
}

fn data_chunks(size: usize) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    futures::stream::unfold(size, |remaining| async move {
        if remaining == 0 {
            return None;
        }
        let count = remaining.min(64 * 1024);
        Some((Ok(bytes::Bytes::from(vec![b'x'; count])), remaining - count))
    })
    .boxed()
}

fn file<'a>(data: BoxStream<'a, Result<Bytes, std::io::Error>>, extension: Option<&str>) -> resources::R2File<'a> {
    FileObject::from_data(data, extension.map(|value| Extension::new(value).unwrap()))
}

async fn lists_are_scoped_to_members(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    seed_members(&db).await;
    let files = r#"[{"key":"second","extension":"pdf"},{"key":"first","extension":null}]"#;
    for (project_id, user_id, revision) in [(1, 1, 7), (1, 2, 2), (2, 1, 5)] {
        resource_file_list::insert(
            &db,
            ResourceFileList {
                project_id,
                user_id,
                files: files.to_owned(),
                revision,
            },
        )
        .await;
    }
    let rows = resource_file_list::rows(&db).await;
    assert_eq!(
        rows.iter().map(|row| (row.project_id, row.user_id, row.revision)).collect::<Vec<_>>(),
        [(1, 1, 7), (1, 2, 2), (2, 1, 5)]
    );
    assert!(rows.iter().all(|row| row.files == files));
    // Invalid inserts use SQL directly because the fixture helper expects success.
    assert!(db.prepare("INSERT INTO resource_file_list VALUES (1, 1, '[]', 1)").run().await.is_err());
    assert!(db.prepare("INSERT INTO resource_file_list VALUES (2, 2, '[]', 1)").run().await.is_err());
    assert_eq!(resource_file_list::rows(&db).await, rows);
}

async fn resource_lists_reject_invalid_extensions(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    resource_file_list::insert(
        &db,
        ResourceFileList {
            project_id: 1,
            user_id: 1,
            files: r#"[{"key":"projects/1/resource-files/users/1/file","extension":"bad/path"}]"#.to_owned(),
            revision: 1,
        },
    )
    .await;
    assert!(matches!(
        project.resource_by_user(user("author")).current_file_list().await,
        Err(BucketError::Backend(_))
    ));
    assert!(matches!(
        project.resource().download("authors/author/1.bad/path").await,
        Err(BucketError::Backend(_))
    ));
    assert!(matches!(
        resources::objects(&bucket.bucket, &db, ProjectId::new(1), resources::ResourceObjectPrefix::Root).await,
        Err(BucketError::Backend(_))
    ));
}

async fn resource_lists_follow_user_ids(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let other_project = bucket.project(ProjectId::new(2));
    let author = project.resource_by_user(user("author"));
    author
        .update(vec![file(data(b"first"), Some("txt")), file(data(b"additional"), Some("txt"))])
        .await
        .unwrap();
    project
        .resource_by_user(user("other"))
        .update(vec![file(data(b"second"), Some("txt"))])
        .await
        .unwrap();
    other_project
        .resource_by_user(user("author"))
        .update(vec![file(data(b"foreign"), Some("txt"))])
        .await
        .unwrap();
    let files = author.current_file_list().await.unwrap();
    db.prepare("UPDATE project_member SET username = 'renamed' WHERE project_id = 1 AND user_id = 1")
        .run()
        .await
        .unwrap();
    // The new name resolves the same owner, so existing file handles can still be used.
    assert!(matches!(author.current_file_list().await, Err(BucketError::InvalidFile)));
    let renamed = project.resource_by_user(user("renamed"));
    renamed.update(files).await.unwrap();
    assert_eq!(renamed.current_file_list().await.unwrap().len(), 2);
    assert_eq!(
        text(project.resource().download("authors/renamed/1.txt").await.unwrap().unwrap()).await,
        "first"
    );
    assert_eq!(
        text(project.resource().download("authors/renamed/2.txt").await.unwrap().unwrap()).await,
        "additional"
    );
    assert!(project.resource().download("authors/author/2.txt").await.unwrap().is_none());
    let objects = resources::objects(&bucket.bucket, &db, ProjectId::new(1), resources::ResourceObjectPrefix::Root)
        .await
        .unwrap();
    assert_eq!(
        objects.keys().map(String::as_str).collect::<Vec<_>>(),
        ["authors/other/1.txt", "authors/renamed/1.txt", "authors/renamed/2.txt"]
    );
    assert_eq!(
        text(project.resource().download("authors/other/1.txt").await.unwrap().unwrap()).await,
        "second"
    );
    assert_eq!(
        text(other_project.resource().download("authors/author/1.txt").await.unwrap().unwrap()).await,
        "foreign"
    );
    let rows = resource_file_list::rows(&db).await;
    assert_eq!(
        rows.iter().map(|row| (row.project_id, row.user_id, row.revision)).collect::<Vec<_>>(),
        [(1, 1, 2), (1, 2, 1), (2, 1, 1)]
    );
    assert!(matches!(
        project.resource_by_user(user("missing")).update(vec![]).await,
        Err(BucketError::InvalidFile)
    ));
}

async fn resource_updates_reject_username_reassignment(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let author = project.resource_by_user(user("author"));
    // Cover both the initial INSERT and an UPDATE of an existing list.
    for existing in [false, true] {
        if existing {
            author.update(vec![file(data(b"original"), Some("txt"))]).await.unwrap();
        }
        let before = resource_file_list::rows(&db).await;
        let stream = futures::stream::once(worker::send::SendFuture::new(async {
            db.batch(vec![
                db.prepare("UPDATE project_member SET username = 'renamed' WHERE project_id = 1 AND user_id = 1"),
                db.prepare("UPDATE project_member SET username = 'author' WHERE project_id = 1 AND user_id = 2"),
            ])
            .await
            .unwrap();
            Ok(Bytes::from_static(b"replacement"))
        }))
        .boxed();
        assert!(matches!(author.update(vec![file(stream, Some("txt"))]).await, Err(BucketError::Conflict)));
        assert_eq!(resource_file_list::rows(&db).await, before);
        assert!(author.current_file_list().await.unwrap().is_empty());
        let renamed = project.resource_by_user(user("renamed"));
        let files = renamed.current_file_list().await.unwrap();
        assert_eq!(files.len(), usize::from(existing));
        if existing {
            assert_eq!(
                text(project.resource().download("authors/renamed/1.txt").await.unwrap().unwrap()).await,
                "original"
            );
            assert!(matches!(author.update(files).await, Err(BucketError::InvalidFile)));
        }
        assert_eq!(bucket.bucket.list().execute().await.unwrap().objects().len(), usize::from(existing));
        db.batch(vec![
            db.prepare("UPDATE project_member SET username = 'other' WHERE project_id = 1 AND user_id = 2"),
            db.prepare("UPDATE project_member SET username = 'author' WHERE project_id = 1 AND user_id = 1"),
        ])
        .await
        .unwrap();
    }
}

async fn resource_lists_insert_remove_and_reorder(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let author = project.resource_by_user(user("author"));
    author.update(vec![file(data(b"existing"), Some("txt"))]).await.unwrap();
    let size = 12 * 1024 * 1024 + 19;
    let borrowed = String::from("borrowed");
    let mut files = author.current_file_list().await.unwrap();
    files.push(file(data_chunks(size), Some("pdf")));
    files.insert(1, file(data(borrowed.as_bytes()), None));
    author.update(files).await.unwrap();
    let resource = project.resource();
    assert_eq!(text(resource.download("authors/author/1.txt").await.unwrap().unwrap()).await, "existing");
    assert_eq!(text(resource.download("authors/author/2").await.unwrap().unwrap()).await, "borrowed");
    let stored = resource.download("authors/author/3.pdf").await.unwrap().unwrap();
    assert_eq!(stored.size, size as u64);
    assert_eq!(
        stored
            .stream
            .try_fold(0_usize, |read, chunk| async move {
                assert!(chunk.iter().all(|byte| *byte == b'x'));
                Ok(read + chunk.len())
            })
            .await
            .unwrap(),
        size
    );
    let before: Vec<_> = bucket
        .bucket
        .list()
        .execute()
        .await
        .unwrap()
        .objects()
        .into_iter()
        .map(|o| (o.key(), o.etag()))
        .collect();
    let mut files = author.current_file_list().await.unwrap();
    files.reverse();
    author.update(files).await.unwrap();
    let after: Vec<_> = bucket
        .bucket
        .list()
        .execute()
        .await
        .unwrap()
        .objects()
        .into_iter()
        .map(|o| (o.key(), o.etag()))
        .collect();
    assert_eq!(before, after, "Reordering must not rewrite file contents");
    assert_eq!(resource.download("authors/author/1.pdf").await.unwrap().unwrap().size, size as u64);
    assert_eq!(text(resource.download("authors/author/3.txt").await.unwrap().unwrap()).await, "existing");
    assert!(resource.download("authors/author/1.txt").await.unwrap().is_none());
    let mut files = author.current_file_list().await.unwrap();
    files.remove(1);
    author.update(files).await.unwrap();
    assert_eq!(text(resource.download("authors/author/2.txt").await.unwrap().unwrap()).await, "existing");
    assert_eq!(bucket.bucket.list().execute().await.unwrap().objects().len(), 2);
    for size in [5 * 1024 * 1024, 0] {
        author.update(vec![file(data_chunks(size), Some("txt"))]).await.unwrap();
        assert_eq!(resource.download("authors/author/1.txt").await.unwrap().unwrap().size, size as u64);
    }
    author.update(vec![]).await.unwrap();
    assert!(author.current_file_list().await.unwrap().is_empty());
    assert!(resource.download("authors/author/1.txt").await.unwrap().is_none());
    assert!(bucket.bucket.list().execute().await.unwrap().objects().is_empty());
}

async fn failed_resource_updates_preserve_existing_files(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let author = project.resource_by_user(user("author"));
    author.update(vec![file(data(b"existing"), Some("txt"))]).await.unwrap();
    let broken = data_chunks(17 * 1024 * 1024).chain(futures::stream::iter([Err(std::io::Error::other("disconnected"))]));
    assert!(matches!(
        author
            .update(vec![file(data(b"completed"), None), file(broken.boxed(), Some("txt"))])
            .await,
        Err(BucketError::Read(_))
    ));
    assert_eq!(
        text(project.resource().download("authors/author/1.txt").await.unwrap().unwrap()).await,
        "existing"
    );
    assert_eq!(bucket.bucket.list().execute().await.unwrap().objects().len(), 1);
    let stale = author.current_file_list().await.unwrap();
    author.update(author.current_file_list().await.unwrap()).await.unwrap();
    assert!(matches!(author.update(stale).await, Err(BucketError::Conflict)));
    let mut duplicate = author.current_file_list().await.unwrap();
    duplicate.extend(author.current_file_list().await.unwrap());
    assert!(matches!(author.update(duplicate).await, Err(BucketError::InvalidFile)));
    let other = project.resource_by_user(user("other"));
    assert!(matches!(
        other.update(author.current_file_list().await.unwrap()).await,
        Err(BucketError::InvalidFile)
    ));
    let other_project = bucket.project(ProjectId::new(2));
    assert!(matches!(
        other_project
            .resource_by_user(user("author"))
            .update(author.current_file_list().await.unwrap())
            .await,
        Err(BucketError::InvalidFile)
    ));
    // A second update winning while the first is reading must not be overwritten.
    let stream = futures::stream::once(async {
        author.update(vec![file(data(b"winner"), Some("txt"))]).await.unwrap();
        Ok(Bytes::from_static(b"loser"))
    })
    .boxed();
    assert!(matches!(author.update(vec![file(stream, Some("txt"))]).await, Err(BucketError::Conflict)));
    assert_eq!(
        text(project.resource().download("authors/author/1.txt").await.unwrap().unwrap()).await,
        "winner"
    );
    assert_eq!(bucket.bucket.list().execute().await.unwrap().objects().len(), 1);
}

async fn stream_uploads_preserve_other_objects_and_failed_replacements(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let resources = project.resource();
    project
        .resource_by_user(user("author"))
        .update(vec![file(data(b"draft"), Some("txt"))])
        .await
        .unwrap();
    // Scoped writes stream across multiple parts.
    let size = 21 * 1024 * 1024 + 19;
    resources.scope.put("streamed.bin", data_chunks(size)).await.unwrap();
    let stored = resources.download("streamed.bin").await.unwrap().unwrap();
    assert_eq!(stored.size, size as u64);
    assert_eq!(
        stored
            .stream
            .try_fold(0_usize, |read, chunk| async move {
                assert!(chunk.iter().all(|byte| *byte == b'x'));
                Ok(read + chunk.len())
            })
            .await
            .unwrap(),
        size
    );
    let broken = || {
        data_chunks(17 * 1024 * 1024)
            .chain(futures::stream::iter([Err(std::io::Error::other("disconnected"))]))
            .boxed()
    };
    assert!(matches!(resources.scope.put("streamed.bin", broken()).await, Err(BucketError::Read(_))));
    assert_eq!(resources.download("streamed.bin").await.unwrap().unwrap().size, size as u64);
    assert_eq!(text(resources.download("authors/author/1.txt").await.unwrap().unwrap()).await, "draft");

    let results = project.result();
    let run_key = key("run");
    let run = ResultListBucket::result(&results, &run_key);
    let filename = String::from("nested/result.txt");
    let contents = String::from("borrowed result");
    run.upload(&filename, data(contents.as_bytes())).await.unwrap();
    run.upload("other.txt", data(b"other")).await.unwrap();
    assert!(matches!(run.upload(&filename, broken()).await, Err(BucketError::Read(_))));
    assert_eq!(text(run.download(&filename).await.unwrap().unwrap()).await, contents);
    assert_eq!(text(run.download("other.txt").await.unwrap().unwrap()).await, "other");
    assert_eq!(run.list().await.unwrap().len(), 2);
}

async fn seed_members(db: &worker::D1Database) {
    for (id, name) in [(1, "alpha"), (2, "beta")] {
        project::insert(db, project::TestProject { id, name: name.into() }).await;
    }
    for (id, username) in [(1, "author"), (2, "other"), (3, "author-extra")] {
        discord_user::insert(
            db,
            discord_user::TestDiscordUser {
                id,
                discord_id: None,
                username: username.to_owned(),
            },
        )
        .await;
    }
    for (project_id, user_id, username) in [(1, 1, "author"), (1, 2, "other"), (1, 3, "author-extra"), (2, 1, "author")] {
        project_member::insert(
            db,
            project_member::TestProjectMember {
                project_id,
                user_id,
                username: username.to_owned(),
                permission_set: 4,
            },
        )
        .await;
    }
}

pub(crate) async fn bucket(env: &Env) -> R2Bucket {
    let db = env.d1("TEST_DB").unwrap();
    seed_members(&db).await;
    let bucket = env.bucket("TEST_BUCKET").unwrap();
    loop {
        let objects = bucket.list().limit(100).execute().await.unwrap().objects();
        if objects.is_empty() {
            break;
        }
        bucket.delete_multiple(objects.iter().map(|o| o.key()).collect()).await.unwrap();
    }
    R2Bucket::new(bucket, db)
}
fn user(name: &str) -> Username {
    Username::new(name.into()).unwrap()
}
fn key(name: &str) -> ResultKey {
    let nonce = name.bytes().fold(0_u64, |nonce, byte| nonce * 128 + u64::from(byte));
    ResultKey::new(format!("998232709354-{nonce:019}")).unwrap()
}
async fn text(data: Data) -> String {
    String::from_utf8(
        data.stream
            .try_fold(Vec::new(), |mut data, chunk| async move {
                data.extend_from_slice(&chunk);
                Ok(data)
            })
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn resources_are_scoped_and_streamed(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let other_project = bucket.project(ProjectId::new(2));
    // The environment is uploaded directly to R2, outside the Worker resource API.
    bucket
        .bucket
        .put("projects/1/resources/environment.closure_zstd", b"closure".to_vec())
        .execute()
        .await
        .unwrap();
    assert_eq!(
        text(project.resource().download("environment.closure_zstd").await.unwrap().unwrap()).await,
        "closure"
    );
    assert!(other_project.resource().download("environment.closure_zstd").await.unwrap().is_none());
    let author = project.resource_by_user(user("author"));
    let similar = project.resource_by_user(user("author-extra"));
    similar.update(vec![file(data(b"other"), Some("txt"))]).await.unwrap();
    other_project
        .resource_by_user(user("author"))
        .update(vec![file(data(b"other project"), Some("txt"))])
        .await
        .unwrap();
    author.update((0..100).map(|_| file(data(b"old"), Some("txt"))).collect()).await.unwrap();
    assert_eq!(author.current_file_list().await.unwrap().len(), 100);
    let unread = || futures::stream::poll_fn(|_| panic!("Invalid lists must not read streams")).boxed();
    assert!(matches!(
        author.update((0..101).map(|_| file(unread(), None)).collect()).await,
        Err(BucketError::TooManyFiles)
    ));
    assert!(matches!(
        author.update(vec![file(unread(), Some(&"x".repeat(1000)))]).await,
        Err(BucketError::InvalidKey)
    ));
    author.update(vec![file(data(b"new"), Some("txt"))]).await.unwrap();
    let objects = bucket.bucket.list().execute().await.unwrap().objects();
    assert_eq!(objects.len(), 4);
    let object = project.resource().download("authors/author/1.txt").await.unwrap().unwrap();
    assert_eq!(object.size, 3);
    assert_eq!(text(object).await, "new");
    assert_eq!(
        text(project.resource().download("authors/author-extra/1.txt").await.unwrap().unwrap()).await,
        "other"
    );
}
async fn user_result_list_requires_visible_files(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let all = project.result();
    let mine = project.result_by_user(user("author"));
    assert!(UserResultListBucket::list(&mine).await.unwrap().is_empty());

    let global_key = key("global");
    for (run, path) in [
        (global_key.clone(), "full.pdf"),
        (key("other"), "authors/other/full.pdf"),
        (key("similar"), "authors/author-extra/full.pdf"),
    ] {
        ResultListBucket::result(&all, &run).upload(path, data(b"result")).await.unwrap();
        assert!(UserResultListBucket::result(&mine, &run).list().await.unwrap().is_empty());
    }
    for (run, path) in [(key("marker"), "authors/author/"), (key("invalid"), "authors/author/../other.pdf")] {
        bucket
            .bucket
            .put(format!("projects/1/results/{}/{path}", run.as_str()), "".to_owned())
            .execute()
            .await
            .unwrap();
    }
    ResultListBucket::result(&bucket.project(ProjectId::new(2)).result(), &key("foreign"))
        .upload("authors/author/proof.pdf", data(b"foreign"))
        .await
        .unwrap();
    assert_eq!(ResultListBucket::list(&all).await.unwrap().len(), 5);
    assert!(UserResultListBucket::list(&mine).await.unwrap().is_empty());

    // A run appears once as soon as it has a file this user can list and download.
    let result = ResultListBucket::result(&all, &global_key);
    result.upload("authors/author/nested/proof.pdf", data(b"proof")).await.unwrap();
    result.upload("authors/author/log.txt", data(b"log")).await.unwrap();
    let runs = UserResultListBucket::list(&mine).await.unwrap();
    assert_eq!(runs.iter().map(|run| &run.key).collect::<Vec<_>>(), [&global_key]);
    let visible = UserResultListBucket::result(&mine, &runs[0].key);
    let files = visible.list().await.unwrap();
    assert_eq!(files.len(), 2);
    for file in files {
        assert!(visible.download(&file.key).await.unwrap().is_some());
    }

    bucket
        .bucket
        .delete(format!("projects/1/results/{}/authors/author/nested/proof.pdf", global_key.as_str()))
        .await
        .unwrap();
    assert_eq!(UserResultListBucket::list(&mine).await.unwrap().len(), 1);
    bucket
        .bucket
        .delete(format!("projects/1/results/{}/authors/author/log.txt", global_key.as_str()))
        .await
        .unwrap();
    assert!(UserResultListBucket::list(&mine).await.unwrap().is_empty());
    // The project-wide result remains present after this user's last file is removed.
    assert_eq!(ResultListBucket::list(&all).await.unwrap().len(), 5);
}

async fn results_paginate_and_isolate_authors(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let all = project.result();
    let mine = project.result_by_user(user("author"));
    // Force several pages before the first matching author and across run prefixes.
    for i in 0..105 {
        ResultListBucket::result(&all, &key("aaa"))
            .upload(&format!("authors/other/{i:03}.txt"), data(b"other"))
            .await
            .unwrap();
        ResultListBucket::result(&all, &key(&format!("{i:03}")))
            .upload("authors/author/one.txt", data(b"own"))
            .await
            .unwrap();
        ResultListBucket::result(&all, &key("run"))
            .upload(&format!("authors/author/{i:03}.txt"), data(b"own"))
            .await
            .unwrap();
    }
    ResultListBucket::result(&all, &key("run"))
        .upload("full.pdf", data(b"full"))
        .await
        .unwrap();
    ResultListBucket::result(&all, &key("run"))
        .upload("authors/author-extra/other.txt", data(b"other"))
        .await
        .unwrap();
    // Invalid/empty author-relative paths must not make a run visible.
    bucket
        .bucket
        .put(format!("projects/1/results/{}/authors/author/", key("empty").as_str()), "".to_owned())
        .execute()
        .await
        .unwrap();
    bucket
        .bucket
        .put(
            format!("projects/1/results/{}/authors/author/../other.txt", key("invalid").as_str()),
            "".to_owned(),
        )
        .execute()
        .await
        .unwrap();
    bucket
        .bucket
        .put(
            format!("projects/2/results/{}/authors/author/file.txt", key("foreign").as_str()),
            "".to_owned(),
        )
        .execute()
        .await
        .unwrap();
    // Raw objects with invalid run IDs must not appear as dated build results.
    bucket
        .bucket
        .put("projects/1/results/undated/authors/author/file.txt", "".to_owned())
        .execute()
        .await
        .unwrap();
    let runs = UserResultListBucket::list(&mine).await.unwrap();
    assert_eq!(runs.len(), 106);
    for (i, run) in runs[..105].iter().enumerate() {
        assert_eq!(run.key, key(&format!("{i:03}")));
    }
    assert_eq!(runs[105].key, key("run"));
    let all_runs = ResultListBucket::list(&all).await.unwrap();
    assert_eq!(all_runs.len(), 109);
    assert!(all_runs.windows(2).all(|runs| runs[0].key < runs[1].key));
    let run_key = key("run");
    let result = UserResultListBucket::result(&mine, &run_key);
    let files = result.list().await.unwrap();
    assert_eq!(files.len(), 105);
    for (index, file) in files.iter().enumerate() {
        assert_eq!(file.key, format!("{:03}.txt", 104 - index));
        assert_eq!(file.size, 3);
    }
    assert_eq!(text(result.download("104.txt").await.unwrap().unwrap()).await, "own");
    assert!(result.download("full.pdf").await.unwrap().is_none());
    assert!(result.download("authors/other/000.txt").await.unwrap().is_none());
    assert!(result.download("missing").await.unwrap().is_none());
}
async fn invalid_keys_do_not_escape_scopes(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let resources = project.resource();
    let results = project.result_by_user(user("author"));
    let run_key = key("run");
    let run = UserResultListBucket::result(&results, &run_key);
    for key in [
        "",
        "../outside",
        "dir/../outside",
        "/outside",
        "dir//file",
        "dir\\file",
        "bad\0",
        &"x".repeat(1024),
    ] {
        assert!(matches!(resources.download(key).await, Err(BucketError::InvalidKey)));
        assert!(matches!(run.download(key).await, Err(BucketError::InvalidKey)));
    }
    assert!(ResultKey::new(format!("998232709354-{}", "1".repeat(1024))).is_none());
    assert!(bucket.bucket.list().execute().await.unwrap().objects().is_empty());
}
async fn generated_keys_preserve_build_time(_env: Env) {
    let now = OffsetDateTime::from_unix_timestamp(1_767_290_646).unwrap();
    let first = generate_key(now).unwrap();
    let second = generate_key(now + time::Duration::SECOND).unwrap();
    assert_eq!(first.built_datetime(), now);
    assert!(second < first);
    assert_ne!(first, generate_key(now).unwrap());
}

async fn stream_uploads_select_single_put_or_larger_parts(env: Env) {
    let bucket = bucket(&env).await;
    let project = bucket.project(ProjectId::new(1));
    let results = project.result();
    let run_key = key("run");
    let result = ResultListBucket::result(&results, &run_key);
    let physical_key = format!("projects/1/results/{}/file.bin", run_key.as_str());
    const PART: usize = 16 * 1024 * 1024;
    for (size, part_count) in [
        (0, None),
        (1, None),
        (PART - 1, None),
        (PART, None),
        (PART + 1, Some(2)),
        (2 * PART, Some(2)),
        (2 * PART + 1, Some(3)),
    ] {
        // Empty chunks do not imply EOF or force multipart at the threshold.
        let stream = data_chunks(size).chain(futures::stream::iter([Ok(Bytes::new())])).boxed();
        result.upload("file.bin", stream).await.unwrap();
        let stored = bucket.bucket.head(&physical_key).await.unwrap().unwrap();
        assert_eq!(stored.size(), size as u64);
        assert_eq!(stored.http_metadata().content_type.as_deref(), Some("application/octet-stream"));
        let etag = stored.etag();
        match part_count {
            Some(count) => assert!(etag.ends_with(&format!("-{count}")), "Expected {count} parts for {size} bytes: {etag}"),
            None => assert!(!etag.contains('-'), "Expected a single PUT for {size} bytes: {etag}"),
        }
        let downloaded = result.download("file.bin").await.unwrap().unwrap();
        assert_eq!(
            downloaded
                .stream
                .try_fold(0_usize, |read, chunk| async move {
                    assert!(chunk.iter().all(|byte| *byte == b'x'));
                    Ok(read + chunk.len())
                })
                .await
                .unwrap(),
            size
        );
    }
    // A single incoming chunk may span several output parts.
    let large_chunk = Bytes::from(vec![b'x'; 2 * PART + 1]);
    result.upload("file.bin", futures::stream::iter([Ok(large_chunk)]).boxed()).await.unwrap();
    let stored = bucket.bucket.head(&physical_key).await.unwrap().unwrap();
    assert_eq!(stored.size(), (2 * PART + 1) as u64);
    assert!(stored.etag().ends_with("-3"));
    let original_etag = stored.etag();
    // A failed stream must preserve the old object on either side of the switch.
    for size in [1, PART, PART + 1] {
        let broken = data_chunks(size)
            .chain(futures::stream::iter([Err(std::io::Error::other("disconnected"))]))
            .boxed();
        assert!(matches!(result.upload("file.bin", broken).await, Err(BucketError::Read(_))));
        assert_eq!(bucket.bucket.head(&physical_key).await.unwrap().unwrap().etag(), original_etag);
    }
}
