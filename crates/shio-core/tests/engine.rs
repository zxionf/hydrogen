//! 引擎端到端用例：真实临时文件、真实 Argon2id 与 AEAD。
//!
//! 为了不拖慢测试，Argon2id 使用小参数（8 MiB、1 轮、1 并行）；参数随库落盘，
//! 重新打开时以落盘值为准。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

use redb::{Database, ReadableTable};
use serde::{Deserialize, Serialize};
use shio_core::codec::encode_record_key;
use shio_core::crypto::kdf::KdfParams;
use shio_core::model::{ListOptions, RecordId, WriteOp};
use shio_core::store::{records, Engine, EngineOptions};
use shio_core::Error;

const PASSWORD: &[u8] = b"correct horse battery staple";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Note {
    title: String,
    body: String,
}

fn note(title: &str) -> Note {
    Note {
        title: title.to_owned(),
        body: format!("body of {title}"),
    }
}

fn fast_kdf() -> KdfParams {
    KdfParams::new(8_192, 1, 1)
}

fn options(path: &Path) -> EngineOptions {
    EngineOptions::new(path).kdf(fast_kdf())
}

fn create(path: &Path) -> Engine {
    Engine::open(options(path).create(true), PASSWORD).expect("create engine")
}

fn open(path: &Path) -> Engine {
    Engine::open(options(path), PASSWORD).expect("open engine")
}

#[test]
fn engine_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Engine>();
}

#[test]
fn roundtrip_returns_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let (id, meta) = engine.insert("notes", "notes", &note("first")).unwrap();
    assert_eq!(meta.id, id);
    assert_eq!(meta.rev, 1);
    assert_eq!(meta.data_version, 1);
    assert!(!meta.deleted);
    assert!(meta.ciphertext_len > 0);
    assert!(meta.created_at <= meta.updated_at);

    let stored = engine.get::<Note>("notes", "notes", &id).unwrap().unwrap();
    assert_eq!(stored.value, note("first"));
    assert_eq!(stored.meta, meta);
}

#[test]
fn reopen_with_same_password_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let (id, meta) = {
        let engine = create(&path);
        let inserted = engine.insert("notes", "notes", &note("persisted")).unwrap();
        engine.flush().unwrap();
        inserted
    };

    let engine = open(&path);
    let stored = engine.get::<Note>("notes", "notes", &id).unwrap().unwrap();
    assert_eq!(stored.value, note("persisted"));
    assert_eq!(stored.meta.rev, meta.rev);

    let stats = engine.stats().unwrap();
    assert_eq!(stats.format_version, 1);
    assert_eq!(stats.key_id, 1);
    assert!(stats.db_bytes > 0);
}

#[test]
fn wrong_password_is_invalid_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    {
        let engine = create(&path);
        engine.insert("notes", "notes", &note("secret")).unwrap();
    }

    let error = Engine::open(options(&path), b"wrong password").unwrap_err();
    assert!(matches!(error, Error::InvalidKey), "{error:?}");
}

#[test]
fn missing_database_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.db");

    let error = Engine::open(options(&path), PASSWORD).unwrap_err();
    assert!(matches!(error, Error::NotFound), "{error:?}");
}

#[test]
fn overwrite_bumps_rev_and_keeps_created_at() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let (id, first) = engine.insert("notes", "notes", &note("v1")).unwrap();
    let second = engine.put("notes", "notes", id, &note("v2"), 2).unwrap();

    assert_eq!(second.rev, first.rev + 1);
    assert_eq!(second.created_at, first.created_at);
    assert_eq!(second.data_version, 2);
    assert!(second.updated_at >= first.updated_at);

    let stored = engine.get::<Note>("notes", "notes", &id).unwrap().unwrap();
    assert_eq!(stored.value, note("v2"));
    assert_eq!(stored.meta.rev, 2);
    assert_eq!(stored.meta.data_version, 2);
}

#[test]
fn delete_writes_tombstone_and_put_resurrects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let (id, _) = engine.insert("notes", "notes", &note("doomed")).unwrap();

    assert!(engine.delete("notes", "notes", &id).unwrap());
    assert!(engine.get::<Note>("notes", "notes", &id).unwrap().is_none());
    assert!(engine
        .list("notes", "notes", ListOptions::default())
        .unwrap()
        .items
        .is_empty());

    let tombstones = engine
        .list(
            "notes",
            "notes",
            ListOptions::default().include_deleted(true),
        )
        .unwrap();
    assert_eq!(tombstones.items.len(), 1);
    assert!(tombstones.items[0].deleted);
    assert_eq!(tombstones.items[0].rev, 2);

    // 再次删除仍写墓碑并递增 rev。
    assert!(engine.delete("notes", "notes", &id).unwrap());

    // 复活：rev 递增，墓碑标记清除。
    let revived = engine.put("notes", "notes", id, &note("alive"), 1).unwrap();
    assert!(!revived.deleted);
    assert_eq!(revived.rev, 4);
    assert_eq!(
        engine
            .get::<Note>("notes", "notes", &id)
            .unwrap()
            .unwrap()
            .value,
        note("alive")
    );
}

#[test]
fn delete_missing_returns_false() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let id = RecordId::new_v7();
    assert!(!engine.delete("notes", "notes", &id).unwrap());
    assert!(!engine.purge("notes", "notes", &id).unwrap());
}

#[test]
fn purge_removes_row_and_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let (id, _) = engine.insert("notes", "notes", &note("gone")).unwrap();
    assert!(engine.delete("notes", "notes", &id).unwrap());
    assert!(engine.purge("notes", "notes", &id).unwrap());
    assert!(!engine.purge("notes", "notes", &id).unwrap());

    let all = engine
        .list(
            "notes",
            "notes",
            ListOptions::default().include_deleted(true),
        )
        .unwrap();
    assert!(all.items.is_empty());
}

#[test]
fn list_paginates_in_id_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let mut ids: Vec<RecordId> = (0..5)
        .map(|index| {
            engine
                .insert("notes", "notes", &note(&format!("n{index}")))
                .unwrap()
                .0
        })
        .collect();
    ids.sort();

    // 另一个集合不应混入。
    engine.insert("notes", "other", &note("elsewhere")).unwrap();

    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let mut list_options = ListOptions::default().with_limit(2);
        if let Some(id) = after {
            list_options = list_options.after(id);
        }
        let page = engine.list("notes", "notes", list_options).unwrap();
        assert!(page.items.len() <= 2);
        seen.extend(page.items.iter().map(|meta| meta.id));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }

    assert_eq!(seen, ids);
}

#[test]
fn list_rejects_bad_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let error = engine
        .list("notes", "notes", ListOptions::default().with_limit(0))
        .unwrap_err();
    assert!(matches!(error, Error::InvalidArgument(_)), "{error:?}");
}

#[test]
fn batch_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let kept = RecordId::new_v7();
    let missing = RecordId::new_v7();

    let ops = vec![
        WriteOp::Put {
            id: kept,
            value: note("kept"),
        },
        WriteOp::Delete { id: missing },
    ];
    let error = engine.batch("notes", "notes", &ops).unwrap_err();
    assert!(matches!(error, Error::NotFound), "{error:?}");

    // 整批回滚：put 没有落盘，也没有墓碑。
    assert!(engine
        .get::<Note>("notes", "notes", &kept)
        .unwrap()
        .is_none());
    let all = engine
        .list(
            "notes",
            "notes",
            ListOptions::default().include_deleted(true),
        )
        .unwrap();
    assert!(all.items.is_empty());

    // 合法批次：两个 put 加一个 delete，返回顺序与输入一致。
    let first = RecordId::new_v7();
    let second = RecordId::new_v7();
    let ops = vec![
        WriteOp::Put {
            id: first,
            value: note("first"),
        },
        WriteOp::Put {
            id: second,
            value: note("second"),
        },
    ];
    let metas = engine.batch("notes", "notes", &ops).unwrap();
    assert_eq!(metas.len(), 2);
    assert_eq!(metas[0].id, first);
    assert_eq!(metas[0].rev, 1);
    assert_eq!(metas[1].id, second);

    let deleted = engine
        .batch("notes", "notes", &[WriteOp::<Note>::Delete { id: first }])
        .unwrap();
    assert_eq!(deleted.len(), 1);
    assert!(deleted[0].deleted);
    assert_eq!(deleted[0].rev, 2);
}

#[test]
fn value_limit_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = Engine::open(options(&path).create(true).max_value_size(32), PASSWORD).unwrap();

    let error = engine
        .insert("notes", "notes", &note("too long"))
        .unwrap_err();
    assert!(
        matches!(error, Error::ValueTooLarge { max: 32, .. }),
        "{error:?}"
    );
}

#[test]
fn invalid_names_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    let error = engine.insert("", "notes", &note("x")).unwrap_err();
    assert!(matches!(error, Error::InvalidName { .. }), "{error:?}");

    let error = engine
        .insert("notes", "Not Allowed", &note("x"))
        .unwrap_err();
    assert!(matches!(error, Error::InvalidName { .. }), "{error:?}");
}

#[test]
fn collections_lists_registered_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");
    let engine = create(&path);

    engine.insert("notes", "alpha", &note("a")).unwrap();
    engine.insert("notes", "beta", &note("b")).unwrap();
    engine.insert("journal", "alpha", &note("c")).unwrap();

    assert_eq!(engine.collections("notes").unwrap(), vec!["alpha", "beta"]);
    assert_eq!(engine.collections("journal").unwrap(), vec!["alpha"]);
    assert!(engine.collections("empty").unwrap().is_empty());
}

#[test]
fn tampered_record_is_corrupted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");

    let id = {
        let engine = create(&path);
        engine
            .insert("notes", "notes", &note("authentic"))
            .unwrap()
            .0
    };

    let key = encode_record_key("notes", "notes", &id).unwrap();
    {
        let db = Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(records::TABLE).unwrap();
            let original = table.get(key.as_slice()).unwrap().unwrap().value().to_vec();
            let mut tampered = original;
            let last = tampered.len() - 1;
            tampered[last] ^= 0x01;
            table.insert(key.as_slice(), tampered.as_slice()).unwrap();
        }
        txn.commit().unwrap();
    }

    let engine = open(&path);
    let error = engine.get::<Note>("notes", "notes", &id).unwrap_err();
    assert!(matches!(error, Error::Corrupted(_)), "{error:?}");
}

#[test]
fn read_only_engine_rejects_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");

    let id = {
        let engine = create(&path);
        engine
            .insert("notes", "notes", &note("readonly"))
            .unwrap()
            .0
    };

    let engine = Engine::open(options(&path).read_only(true), PASSWORD).unwrap();
    assert_eq!(
        engine
            .get::<Note>("notes", "notes", &id)
            .unwrap()
            .unwrap()
            .value,
        note("readonly")
    );
    assert_eq!(
        engine
            .list("notes", "notes", ListOptions::default())
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(engine.stats().unwrap().key_id, 1);

    assert!(matches!(
        engine.put("notes", "notes", RecordId::new_v7(), &note("x"), 1),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        engine.delete("notes", "notes", &id),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        engine.purge("notes", "notes", &id),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        engine.batch("notes", "notes", &[WriteOp::<Note>::Delete { id }]),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(
        engine.change_password(b"next"),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(engine.rotate_data_key(), Err(Error::ReadOnly)));
}

#[test]
fn read_only_with_create_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");

    let error = Engine::open(options(&path).create(true).read_only(true), PASSWORD).unwrap_err();
    assert!(matches!(error, Error::InvalidArgument(_)), "{error:?}");
}

#[test]
fn change_password_rewraps_and_keeps_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");

    let id = {
        let engine = create(&path);
        let (id, meta) = engine.insert("notes", "notes", &note("secret")).unwrap();
        engine.change_password(b"new password").unwrap();

        let stored = engine.get::<Note>("notes", "notes", &id).unwrap().unwrap();
        assert_eq!(stored.value, note("secret"));
        assert_eq!(stored.meta.rev, meta.rev);

        // 同一会话可以连续改口令。
        engine.change_password(b"third password").unwrap();
        id
    };

    let error = Engine::open(options(&path), PASSWORD).unwrap_err();
    assert!(matches!(error, Error::InvalidKey), "{error:?}");

    let engine = Engine::open(options(&path), b"third password").unwrap();
    assert_eq!(
        engine
            .get::<Note>("notes", "notes", &id)
            .unwrap()
            .unwrap()
            .value,
        note("secret")
    );
    assert_eq!(engine.stats().unwrap().key_id, 1);
}

#[test]
fn rotate_data_key_migrates_all_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");

    let ids = {
        let engine = create(&path);
        let ids: Vec<RecordId> = (0..300).map(|_| RecordId::new_v7()).collect();
        let ops: Vec<WriteOp<Note>> = ids
            .iter()
            .map(|id| WriteOp::Put {
                id: *id,
                value: note("bulk"),
            })
            .collect();
        engine.batch("notes", "notes", &ops).unwrap();
        assert_eq!(engine.stats().unwrap().key_id, 1);

        // 300 条跨过轮换的批边界（每批 256 条）。
        engine.rotate_data_key().unwrap();
        assert_eq!(engine.stats().unwrap().key_id, 2);
        for id in &ids {
            assert_eq!(
                engine
                    .get::<Note>("notes", "notes", id)
                    .unwrap()
                    .unwrap()
                    .value,
                note("bulk")
            );
        }
        ids
    };

    let engine = open(&path);
    assert_eq!(engine.stats().unwrap().key_id, 2);
    for id in &ids {
        assert!(engine.get::<Note>("notes", "notes", id).unwrap().is_some());
    }

    // 旧 key 行已删除，因此可以改口令。
    engine.change_password(b"after rotation").unwrap();
    drop(engine);

    let engine = Engine::open(options(&path), b"after rotation").unwrap();
    assert_eq!(engine.stats().unwrap().key_id, 2);
    assert_eq!(
        engine
            .get::<Note>("notes", "notes", &ids[0])
            .unwrap()
            .unwrap()
            .value,
        note("bulk")
    );
}

#[test]
fn interrupted_rotation_is_resumable_and_blocks_change_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shio.db");

    let (first, second) = {
        let engine = create(&path);
        let first = engine.insert("notes", "notes", &note("a")).unwrap().0;
        let second = engine.insert("notes", "notes", &note("b")).unwrap().0;
        (first, second)
    };

    // 篡改一条记录，让第一次轮换在迁移中失败，留下两行 key。
    let mut originals = Vec::new();
    {
        let db = Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(records::TABLE).unwrap();
            for entry in table.iter().unwrap() {
                let (key, value) = entry.unwrap();
                originals.push((key.value().to_vec(), value.value().to_vec()));
            }
            let (key, value) = &originals[0];
            let mut tampered = value.clone();
            let last = tampered.len() - 1;
            tampered[last] ^= 0x01;
            table.insert(key.as_slice(), tampered.as_slice()).unwrap();
        }
        txn.commit().unwrap();
    }

    let engine = open(&path);
    let error = engine.rotate_data_key().unwrap_err();
    assert!(matches!(error, Error::Corrupted(_)), "{error:?}");
    assert!(matches!(
        engine.change_password(b"blocked"),
        Err(Error::Busy)
    ));
    drop(engine);

    // 还原记录后重开：应同时解包两行 key，并复用已写入的新 key 完成迁移。
    {
        let db = Database::open(&path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(records::TABLE).unwrap();
            for (key, value) in &originals {
                table.insert(key.as_slice(), value.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }

    let engine = open(&path);
    assert_eq!(
        engine
            .get::<Note>("notes", "notes", &first)
            .unwrap()
            .unwrap()
            .value,
        note("a")
    );
    assert_eq!(
        engine
            .get::<Note>("notes", "notes", &second)
            .unwrap()
            .unwrap()
            .value,
        note("b")
    );

    engine.rotate_data_key().unwrap();
    assert_eq!(engine.stats().unwrap().key_id, 2);
    engine.change_password(b"resumed").unwrap();
    drop(engine);

    let engine = Engine::open(options(&path), b"resumed").unwrap();
    assert_eq!(engine.stats().unwrap().key_id, 2);
    assert!(engine
        .get::<Note>("notes", "notes", &first)
        .unwrap()
        .is_some());
}
