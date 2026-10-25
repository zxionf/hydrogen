//! facade 端到端用例：异步入口、句柄、只读闸门与关闭语义。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use shio::{Credential, Error, Kernel, ListOptions, Namespace, OpenOptions, RecordId, WriteOp};
const PASSWORD: &str = "correct horse battery staple";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Note {
    title: String,
}

fn note(title: &str) -> Note {
    Note {
        title: title.to_owned(),
    }
}

fn create(path: &Path) -> OpenOptions {
    OpenOptions::new(path).create(true)
}

fn collection(kernel: &Kernel) -> shio::Collection<Note> {
    kernel
        .namespace("notes")
        .unwrap()
        .collection::<Note>("note")
        .unwrap()
}

#[test]
fn handles_are_send_sync() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<Kernel>();
    assert_send_sync::<Namespace>();
    assert_send_sync::<shio::Collection<Note>>();
}

#[tokio::test]
async fn end_to_end_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.redb");

    let kernel = Kernel::open(create(&path), Credential::password(PASSWORD))
        .await
        .unwrap();
    assert!(kernel.stats().unwrap().db_bytes > 0);

    // 名称校验发生在句柄构造时，不访问磁盘。
    assert!(matches!(
        kernel.namespace("Bad Name"),
        Err(Error::InvalidName { .. })
    ));
    let namespace = kernel.namespace("notes").unwrap();
    assert!(matches!(
        namespace.collection::<Note>("bad/name"),
        Err(Error::InvalidName { .. })
    ));

    let notes = collection(&kernel);
    let (id, meta) = notes.insert(&note("first")).await.unwrap();
    assert_eq!(meta.rev, 1);
    assert_eq!(notes.get(&id).await.unwrap().unwrap().value, note("first"));

    let second = notes.put(id, &note("second")).await.unwrap();
    assert_eq!(second.rev, 2);

    let other = RecordId::new_v7();
    let metas = notes
        .batch(vec![
            WriteOp::Put {
                id: other,
                value: note("batch"),
            },
            WriteOp::Delete { id },
        ])
        .await
        .unwrap();
    assert_eq!(metas.len(), 2);
    assert!(metas[1].deleted);
    assert!(notes.get(&id).await.unwrap().is_none());

    let page = notes.list(ListOptions::default()).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, other);
    assert_eq!(
        namespace.collections().await.unwrap(),
        vec!["note".to_owned()]
    );

    // 墓碑不计入默认列表，但可以显式查看。
    let all = notes
        .list(ListOptions::default().include_deleted(true))
        .await
        .unwrap();
    assert_eq!(all.items.len(), 2);

    kernel.flush().await.unwrap();
    kernel.close().await.unwrap();
    kernel.close().await.unwrap();

    assert!(matches!(
        notes.insert(&note("after close")).await,
        Err(Error::Closed)
    ));
    assert!(matches!(
        notes.list(ListOptions::default()).await,
        Err(Error::Closed)
    ));
    assert!(matches!(kernel.stats(), Err(Error::Closed)));

    // 重开：数据仍在，且句柄可以继续使用。
    let kernel = Kernel::open(OpenOptions::new(&path), Credential::password(PASSWORD))
        .await
        .unwrap();
    let notes = collection(&kernel);
    assert_eq!(
        notes.get(&other).await.unwrap().unwrap().value,
        note("batch")
    );
    assert_eq!(
        notes
            .list(ListOptions::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );

    let id = notes.insert(&note("reopened")).await.unwrap().0;
    assert!(notes.purge(&id).await.unwrap());
    assert!(notes.get(&id).await.unwrap().is_none());
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn wrong_password_and_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.redb");

    // 参数校验不访问磁盘。
    assert!(matches!(
        Kernel::open(OpenOptions::new(""), Credential::password(PASSWORD)).await,
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        Kernel::open(
            OpenOptions::new(&path).create(true).read_only(true),
            Credential::password(PASSWORD)
        )
        .await,
        Err(Error::InvalidArgument(_))
    ));

    // 文件不存在且未要求创建。
    let error = Kernel::open(OpenOptions::new(&path), Credential::password(PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::NotFound), "{error:?}");

    {
        let kernel = Kernel::open(create(&path), Credential::password(PASSWORD))
            .await
            .unwrap();
        kernel.close().await.unwrap();
    }

    let error = Kernel::open(OpenOptions::new(&path), Credential::password("wrong"))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidKey), "{error:?}");
}

#[tokio::test]
async fn read_only_rejects_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.redb");

    {
        let kernel = Kernel::open(create(&path), Credential::password(PASSWORD))
            .await
            .unwrap();
        collection(&kernel).insert(&note("seed")).await.unwrap();
        kernel.close().await.unwrap();
    }

    let kernel = Kernel::open(
        OpenOptions::new(&path).read_only(true),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let notes = collection(&kernel);

    assert!(matches!(
        notes.insert(&note("nope")).await,
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        notes.delete(&RecordId::new_v7()).await,
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        notes.batch(Vec::new()).await,
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        kernel.change_password(SecretString::from("nope")).await,
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        kernel.rotate_data_key().await,
        Err(Error::ReadOnly)
    ));

    let page = notes.list(ListOptions::default()).await.unwrap();
    assert_eq!(page.items.len(), 1);
    let stored = notes.get(&page.items[0].id).await.unwrap().unwrap();
    assert_eq!(stored.value, note("seed"));

    kernel.close().await.unwrap();
}

#[tokio::test]
async fn change_password_and_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.redb");

    let id = {
        let kernel = Kernel::open(create(&path), Credential::password(PASSWORD))
            .await
            .unwrap();
        let notes = collection(&kernel);
        let (id, _) = notes.insert(&note("survives")).await.unwrap();
        assert_eq!(kernel.stats().unwrap().key_id, 1);

        kernel
            .change_password(SecretString::from("new password"))
            .await
            .unwrap();
        assert_eq!(
            notes.get(&id).await.unwrap().unwrap().value,
            note("survives")
        );
        kernel.close().await.unwrap();
        id
    };

    assert!(matches!(
        Kernel::open(OpenOptions::new(&path), Credential::password(PASSWORD)).await,
        Err(Error::InvalidKey)
    ));

    let kernel = Kernel::open(
        OpenOptions::new(&path),
        Credential::password("new password"),
    )
    .await
    .unwrap();
    let notes = collection(&kernel);
    assert_eq!(
        notes.get(&id).await.unwrap().unwrap().value,
        note("survives")
    );

    kernel.rotate_data_key().await.unwrap();
    assert_eq!(kernel.stats().unwrap().key_id, 2);
    assert_eq!(
        notes.get(&id).await.unwrap().unwrap().value,
        note("survives")
    );

    // 轮换后旧 key 行已删除，仍可继续改口令。
    kernel
        .change_password(SecretString::from("third password"))
        .await
        .unwrap();
    kernel.close().await.unwrap();

    let kernel = Kernel::open(
        OpenOptions::new(&path),
        Credential::password("third password"),
    )
    .await
    .unwrap();
    assert_eq!(kernel.stats().unwrap().key_id, 2);
    assert_eq!(
        collection(&kernel).get(&id).await.unwrap().unwrap().value,
        note("survives")
    );
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn compact_file_requires_closed_handles_and_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compact.redb");
    let kernel = Kernel::open(create(&path), Credential::password(PASSWORD))
        .await
        .unwrap();
    let notes = collection(&kernel);
    let mut keep = Vec::new();
    for index in 0..60 {
        let id = RecordId::new_v7();
        notes.put(id, &note(&format!("v{index}"))).await.unwrap();
        if index % 3 == 0 {
            keep.push((id, note(&format!("v{index}"))));
        }
    }

    // 句柄未关闭时旁路锁被占用。
    assert!(matches!(
        shio::compact_file(path.clone()).await,
        Err(Error::Locked)
    ));
    kernel.close().await.unwrap();
    drop(kernel);
    drop(notes);

    let report = shio::compact_file(path.clone()).await.unwrap();
    assert!(report.compacted);
    assert!(report.after_bytes <= report.before_bytes);

    let kernel = Kernel::open(create(&path), Credential::password(PASSWORD))
        .await
        .unwrap();
    let notes = collection(&kernel);
    for (id, value) in &keep {
        assert_eq!(notes.get(id).await.unwrap().unwrap().value, *value);
    }
    kernel.close().await.unwrap();
    drop(kernel);
    drop(notes);

    assert!(matches!(
        shio::compact_file(dir.path().join("missing.redb")).await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn export_and_import_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let source = Kernel::open(
        create(&dir.path().join("source.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let notes = collection(&source);
    let id = RecordId::new_v7();
    notes.insert(&note("明文导出")).await.unwrap();
    notes.put(id, &note("second")).await.unwrap();

    let file = dir.path().join("notes.jsonl");
    let info = source
        .export_records(file.clone(), shio::ExportOptions::default())
        .await
        .unwrap();
    assert_eq!(info.records, 2);
    assert_eq!(info.deleted, 0);

    let target = Kernel::open(
        create(&dir.path().join("target.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let target_notes = collection(&target);
    let preview = target
        .import_records(
            file.clone(),
            shio::ImportOptions {
                dry_run: true,
                ..shio::ImportOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(preview.applied, 2);
    assert!(target_notes.get(&id).await.unwrap().is_none());

    let report = target
        .import_records(file.clone(), shio::ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.applied, 2);
    assert_eq!(
        target_notes.get(&id).await.unwrap().unwrap().value,
        note("second")
    );
    assert_eq!(
        target_notes
            .list(ListOptions::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );

    // 目标已存在时默认不覆盖。
    assert!(matches!(
        source
            .export_records(file, shio::ExportOptions::default())
            .await,
        Err(Error::InvalidArgument(_))
    ));

    source.close().await.unwrap();
    target.close().await.unwrap();
}
