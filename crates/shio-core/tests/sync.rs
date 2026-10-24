//! 同步、升级、配对与轮换的端到端用例。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use shio_core::crypto::kdf::KdfParams;
use shio_core::model::RecordId;
use shio_core::store::{Engine, EngineOptions};
use shio_core::{ChangeEntry, DeviceId, Error, KeyId, MigrationOptions};

const PASSWORD: &[u8] = b"correct horse battery staple";

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

fn enable(engine: &Engine) -> shio_core::SyncInfo {
    engine.enable_sync(None).expect("enable sync")
}

fn pair(source: &Engine, path: &Path) -> Engine {
    let (frame, secret) = source
        .create_pairing_bundle_with_secret(std::time::Duration::from_secs(600))
        .expect("pairing bundle");
    Engine::open_with_pairing(options(path), PASSWORD, &frame, &secret).expect("open paired")
}

fn drain(source: &Engine, cursor: &mut u64, target: &Engine) {
    loop {
        let page = source.changes_since(*cursor, 1000).expect("changes");
        let more = page.has_more;
        let next = page.next;
        target.apply_page(&page).expect("apply page");
        *cursor = next;
        if !more {
            break;
        }
    }
}

#[test]
fn enable_sync_upgrades_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.redb");
    let engine = create(&path);
    engine
        .put("notes", "note", RecordId::new_v7(), &"before", 1)
        .unwrap();
    let info = enable(&engine);
    assert!(info.baseline_done);
    assert_eq!(engine.stats().unwrap().format_version, 2);
    assert!(info.log_end >= 2);
    assert_eq!(info.device_id, info.device_id);

    let page = engine.changes_since(0, 1000).unwrap();
    assert!(page
        .entries
        .iter()
        .any(|entry| matches!(entry, ChangeEntry::Record(_))));
    assert!(page
        .entries
        .iter()
        .any(|entry| matches!(entry, ChangeEntry::CurrentKey { .. })));
    drop(engine);

    let reopened = open(&path);
    assert_eq!(reopened.sync_info().unwrap().db_id, info.db_id);
    assert_eq!(reopened.stats().unwrap().format_version, 2);
}

#[test]
fn two_devices_converge_with_lww_and_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    enable(&a);
    let b = pair(&a, &dir.path().join("b.redb"));
    assert!(b.sync_info().unwrap().db_id == a.sync_info().unwrap().db_id);

    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"one", 1).unwrap();
    let mut cursor_a = 0;
    drain(&a, &mut cursor_a, &b);
    assert_eq!(
        b.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "one"
    );

    std::thread::sleep(std::time::Duration::from_millis(2));
    b.put("notes", "note", id, &"two", 1).unwrap();
    let mut cursor_b = 0;
    drain(&b, &mut cursor_b, &a);
    assert_eq!(
        a.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "two"
    );

    std::thread::sleep(std::time::Duration::from_millis(2));
    a.delete("notes", "note", &id).unwrap();
    drain(&a, &mut cursor_a, &b);
    assert!(b.get::<String>("notes", "note", &id).unwrap().is_none());
    assert!(b
        .list("notes", "note", shio_core::model::ListOptions::default())
        .unwrap()
        .items
        .is_empty());

    std::thread::sleep(std::time::Duration::from_millis(2));
    b.put("notes", "note", id, &"three", 1).unwrap();
    drain(&b, &mut cursor_b, &a);
    assert_eq!(
        a.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "three"
    );
}

#[test]
fn snapshot_and_compaction_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    enable(&a);
    let b = pair(&a, &dir.path().join("b.redb"));

    for index in 0..10 {
        a.put(
            "notes",
            "note",
            RecordId::new_v7(),
            &format!("value-{index}"),
            1,
        )
        .unwrap();
    }
    let at = a.snapshot_start().unwrap();
    let page = a.snapshot_page(None, 4).unwrap();
    assert_eq!(page.at_seq, at);
    assert_eq!(page.entries.len(), 4);
    assert!(page.next_key.is_some());
    b.apply_snapshot_page(&page).unwrap();
    let mut after = page.next_key;
    while let Some(key) = after {
        let page = a.snapshot_page(Some(&key), 4).unwrap();
        b.apply_snapshot_page(&page).unwrap();
        after = page.next_key;
    }
    assert_eq!(
        b.list("notes", "note", shio_core::model::ListOptions::default())
            .unwrap()
            .items
            .len(),
        10
    );

    let end = a.sync_info().unwrap().log_end;
    let removed = a.compact_changes(end).unwrap();
    assert!(removed > 0);
    assert!(matches!(
        a.changes_since(0, 100),
        Err(Error::SyncResyncRequired { .. })
    ));
}

#[test]
fn rotation_installs_keys_and_converges() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    enable(&a);
    let b = pair(&a, &dir.path().join("b.redb"));

    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"before", 1).unwrap();
    let mut cursor_a = 0;
    drain(&a, &mut cursor_a, &b);

    let old = a.sync_info().unwrap().current_key_id;
    a.rotate_data_key().unwrap();
    let new = a.sync_info().unwrap().current_key_id;
    assert_ne!(old, new);
    drain(&a, &mut cursor_a, &b);
    assert_eq!(b.sync_info().unwrap().current_key_id, new);
    assert_eq!(
        b.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "before"
    );

    let mut cursor_b = 0;
    drain(&b, &mut cursor_b, &a);
    let b2 = RecordId::new_v7();
    b.put("notes", "note", b2, &"from-b", 1).unwrap();
    drain(&b, &mut cursor_b, &a);
    assert_eq!(
        a.get::<String>("notes", "note", &b2)
            .unwrap()
            .unwrap()
            .value,
        "from-b"
    );

    let state = a.rotation_state().unwrap().expect("rotation state");
    assert_eq!(state.to, new);
    assert!(state.acked.contains(&b.sync_info().unwrap().device_id));

    b.migrate_all(new).unwrap();
    drain(&b, &mut cursor_b, &a);
    a.retire_key(old).unwrap();
    a.prune_rotation_offers(new).unwrap();
    assert!(a.rotation_offers().unwrap().is_empty());
}

#[test]
fn migration_reports_progress_blocks_reads_and_resumes_after_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(create(&dir.path().join("a.redb")));
    enable(&engine);

    let mut ids = Vec::new();
    for index in 0..30 {
        let id = RecordId::new_v7();
        ids.push(id);
        engine
            .put("notes", "note", id, &format!("value-{index}"), 1)
            .unwrap();
    }
    let old = engine.sync_info().unwrap().current_key_id;
    let to = engine.begin_rotation().unwrap().to;
    let options = MigrationOptions {
        batch_size: 1,
        pause: Duration::from_millis(20),
    };

    let worker_engine = Arc::clone(&engine);
    let worker = std::thread::spawn(move || worker_engine.migrate_all_with_options(to, options));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = engine.migration_progress();
        if progress.active && progress.migrated >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "migration did not make progress: {progress:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    let (sender, receiver) = mpsc::channel();
    let reader_engine = Arc::clone(&engine);
    let read_id = ids[0];
    let reader = std::thread::spawn(move || {
        let result = reader_engine.get::<String>("notes", "note", &read_id);
        sender.send(result.is_ok()).unwrap();
    });
    std::thread::sleep(Duration::from_millis(30));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    engine.cancel_migration();
    assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
    reader.join().unwrap();
    assert!(receiver.recv().unwrap());

    let progress = engine.migration_progress();
    assert!(!progress.active);
    assert!(!progress.done);
    assert!(progress.scanned >= progress.migrated);
    assert!(progress.migrated >= 1);

    let remaining = engine.migrate_all(to).unwrap();
    assert!(remaining > 0);
    let progress = engine.migration_progress();
    assert!(!progress.active);
    assert!(progress.done);
    assert_eq!(progress.migrated, remaining);
    assert!(progress.scanned >= progress.migrated);

    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            engine
                .get::<String>("notes", "note", id)
                .unwrap()
                .unwrap()
                .value,
            format!("value-{index}")
        );
    }
    engine.retire_key(old).unwrap();
}

#[test]
fn migration_resumes_from_persisted_cursor_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.redb");
    let engine = Arc::new(create(&path));
    enable(&engine);

    let mut ids = Vec::new();
    for index in 0..8 {
        let id = RecordId::new_v7();
        ids.push(id);
        engine
            .put("notes", "note", id, &format!("value-{index}"), 1)
            .unwrap();
    }
    let to = engine.begin_rotation().unwrap().to;
    let migration_options = MigrationOptions {
        batch_size: 1,
        pause: Duration::from_millis(50),
    };

    let worker_engine = Arc::clone(&engine);
    let worker =
        std::thread::spawn(move || worker_engine.migrate_all_with_options(to, migration_options));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = engine.migration_progress();
        if progress.active && progress.migrated >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "migration did not make progress: {progress:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    engine.cancel_migration();
    assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
    let scanned_before_reopen = engine.migration_progress().scanned;
    assert!(scanned_before_reopen < ids.len() as u64);
    drop(engine);

    let reopened = Engine::open(options(&path), PASSWORD).unwrap();
    let remaining = reopened
        .migrate_all_with_options(
            to,
            MigrationOptions {
                batch_size: 1,
                pause: Duration::ZERO,
            },
        )
        .unwrap();
    assert!(remaining > 0);
    let progress = reopened.migration_progress();
    assert!(progress.done);
    assert_eq!(progress.migrated, remaining);
    assert_eq!(progress.scanned + scanned_before_reopen, ids.len() as u64);

    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            reopened
                .get::<String>("notes", "note", id)
                .unwrap()
                .unwrap()
                .value,
            format!("value-{index}")
        );
    }
}

#[test]
fn remote_record_write_invalidates_migration_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let a = Arc::new(create(&dir.path().join("a.redb")));
    enable(&a);
    let mut ids = Vec::new();
    for index in 0..8 {
        let id = RecordId::new_v7();
        ids.push(id);
        a.put("notes", "note", id, &format!("local-{index}"), 1)
            .unwrap();
    }
    let old = a.sync_info().unwrap().current_key_id;
    let b = pair(&a, &dir.path().join("b.redb"));
    let mut cursor_b = 0;
    drain(&a, &mut cursor_b, &b);

    let to = a.begin_rotation().unwrap().to;
    let migration_options = MigrationOptions {
        batch_size: 1,
        pause: Duration::from_millis(50),
    };
    let worker_a = Arc::clone(&a);
    let worker =
        std::thread::spawn(move || worker_a.migrate_all_with_options(to, migration_options));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = a.migration_progress();
        if progress.active && progress.migrated >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "migration did not make progress: {progress:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    a.cancel_migration();
    assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));

    // B 还没收到轮换，写入仍使用旧 key；这些记录可能落在 A 的迁移游标之前。
    std::thread::sleep(Duration::from_millis(5));
    for (index, id) in ids.iter().enumerate() {
        b.put("notes", "note", *id, &format!("remote-{index}"), 1)
            .unwrap();
    }
    drain(&b, &mut cursor_b, &a);

    a.migrate_all(to).unwrap();
    let progress = a.migration_progress();
    assert!(progress.done);
    assert_eq!(progress.scanned, ids.len() as u64);
    assert_eq!(progress.migrated, ids.len() as u64);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            a.get::<String>("notes", "note", id).unwrap().unwrap().value,
            format!("remote-{index}")
        );
    }
    a.retire_key(old).unwrap();
}

#[test]
fn repeated_rotation_resets_progress() {
    let dir = tempfile::tempdir().unwrap();
    let engine = create(&dir.path().join("a.redb"));
    enable(&engine);
    let id = RecordId::new_v7();
    engine.put("notes", "note", id, &"value", 1).unwrap();

    for _ in 0..2 {
        engine.rotate_data_key().unwrap();
        let progress = engine.migration_progress();
        assert!(!progress.active);
        assert!(progress.done);
        assert!(progress.migrated >= 1);
        assert_eq!(
            engine
                .get::<String>("notes", "note", &id)
                .unwrap()
                .unwrap()
                .value,
            "value"
        );
    }
}

#[test]
fn migration_rejects_out_of_range_options() {
    let dir = tempfile::tempdir().unwrap();
    let engine = create(&dir.path().join("a.redb"));
    enable(&engine);
    let to = engine.begin_rotation().unwrap().to;

    for options in [
        MigrationOptions {
            batch_size: 0,
            pause: Duration::ZERO,
        },
        MigrationOptions {
            batch_size: shio_core::MAX_MIGRATION_BATCH_SIZE + 1,
            pause: Duration::ZERO,
        },
        MigrationOptions {
            batch_size: 1,
            pause: Duration::from_millis(shio_core::MAX_MIGRATION_PAUSE_MS + 1),
        },
    ] {
        assert!(matches!(
            engine.migrate_all_with_options(to, options),
            Err(Error::InvalidArgument(_))
        ));
    }
}

#[test]
fn paired_after_rotation_acks_installed_offer() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    enable(&a);
    a.put("notes", "note", RecordId::new_v7(), &"value", 1)
        .unwrap();
    a.rotate_data_key().unwrap();

    let b = pair(&a, &dir.path().join("b.redb"));
    let mut cursor_a = 0;
    drain(&a, &mut cursor_a, &b);
    let mut cursor_b = 0;
    drain(&b, &mut cursor_b, &a);

    let state = a.rotation_state().unwrap().expect("rotation state");
    assert!(state.acked.contains(&b.sync_info().unwrap().device_id));
}

#[test]
fn sync_change_password_rewraps_all_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.redb");
    let a = create(&path);
    enable(&a);
    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"value", 1).unwrap();
    a.rotate_data_key().unwrap();
    assert!(a.stats().unwrap().key_id != KeyId::from_bytes([0u8; 16]));

    a.change_password(b"new password").unwrap();
    assert_eq!(
        a.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "value"
    );
    drop(a);

    assert!(Engine::open(options(&path), PASSWORD).is_err());
    let reopened = Engine::open(options(&path), b"new password").unwrap();
    assert_eq!(
        reopened
            .get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "value"
    );
}

#[test]
fn paired_device_carries_key_chain_and_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    let device = DeviceId::from_bytes([9u8; 16]);
    let info = a.enable_sync(Some(device)).unwrap();
    assert_eq!(info.device_id, device);

    let mut ids = Vec::new();
    for index in 0..3 {
        let id = RecordId::new_v7();
        ids.push(id);
        a.put("notes", "note", id, &format!("v{index}"), 1).unwrap();
    }
    a.rotate_data_key().unwrap();

    let b = pair(&a, &dir.path().join("b.redb"));
    assert_eq!(b.sync_info().unwrap().db_id, info.db_id);
    let key_id = b.sync_info().unwrap().current_key_id;
    let _: KeyId = key_id;
    let mut cursor = 0;
    drain(&a, &mut cursor, &b);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            b.get::<String>("notes", "note", id).unwrap().unwrap().value,
            format!("v{index}")
        );
    }

    b.put("notes", "note", RecordId::new_v7(), &"from-paired", 1)
        .unwrap();
    let mut cursor = 0;
    drain(&b, &mut cursor, &a);
    assert!(a.get::<String>("notes", "note", &ids[0]).unwrap().is_some());
}

#[test]
fn read_only_and_purge_guards() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.redb");
    let a = create(&path);
    enable(&a);
    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"value", 1).unwrap();
    assert!(matches!(
        a.purge("notes", "note", &id),
        Err(Error::SyncEnabled)
    ));
    assert!(a.batch_raw("notes", "note", &[]).is_ok());
    drop(a);

    let readonly = Engine::open(options(&path).read_only(true), PASSWORD).unwrap();
    assert!(matches!(readonly.enable_sync(None), Err(Error::ReadOnly)));
    assert!(matches!(readonly.compact_changes(1), Err(Error::ReadOnly)));
}

#[test]
fn backup_and_restore_preserve_format1_data() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("source.redb");
    let backup_path = dir.path().join("backup.redb");
    let target_path = dir.path().join("target.redb");
    let source = create(&source_path);
    let id = RecordId::new_v7();
    source.put("notes", "note", id, &"format-one", 1).unwrap();

    let info = source.backup(&backup_path).unwrap();
    assert_eq!(info.format_version, 1);
    assert!(info.db_bytes > 0);
    assert!(info.table_count >= 4);
    drop(source);

    let restored = Engine::restore(&backup_path, options(&target_path), PASSWORD).unwrap();
    assert_eq!(restored.stats().unwrap().format_version, 1);
    assert_eq!(
        restored
            .get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "format-one"
    );
}

#[test]
fn backup_and_restore_preserve_sync_identity() {
    let dir = tempfile::tempdir().unwrap();
    let source = create(&dir.path().join("source.redb"));
    let info = enable(&source);
    let id = RecordId::new_v7();
    source.put("notes", "note", id, &"format-two", 1).unwrap();
    let before = source.sync_info().unwrap();
    assert_eq!(before.db_id, info.db_id);

    let backup_path = dir.path().join("backup.redb");
    let backup = source.backup(&backup_path).unwrap();
    assert_eq!(backup.format_version, 2);
    assert!(backup.row_count >= 1);
    drop(source);

    let target_path = dir.path().join("target.redb");
    let restored = Engine::restore(&backup_path, options(&target_path), PASSWORD).unwrap();
    let after = restored.sync_info().unwrap();
    assert_eq!(after.db_id, before.db_id);
    assert_eq!(after.device_id, before.device_id);
    assert_eq!(after.current_key_id, before.current_key_id);
    assert_eq!(after.log_start, before.log_start);
    assert_eq!(after.log_end, before.log_end);
    assert_eq!(
        restored
            .get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "format-two"
    );
}

#[test]
fn backup_rejects_existing_target_and_restore_rejects_nonempty_target() {
    let dir = tempfile::tempdir().unwrap();
    let source = create(&dir.path().join("source.redb"));
    source
        .put("notes", "note", RecordId::new_v7(), &"value", 1)
        .unwrap();

    let backup_path = dir.path().join("backup.redb");
    std::fs::write(&backup_path, b"already here").unwrap();
    assert!(matches!(
        source.backup(&backup_path),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(std::fs::read(&backup_path).unwrap(), b"already here");

    source.backup(&dir.path().join("good.redb")).unwrap();
    let target_path = dir.path().join("target.redb");
    std::fs::write(&target_path, b"do not overwrite").unwrap();
    assert!(matches!(
        Engine::restore(
            &dir.path().join("good.redb"),
            options(&target_path),
            PASSWORD
        ),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(std::fs::read(&target_path).unwrap(), b"do not overwrite");
}

#[test]
fn restore_accepts_empty_target_and_cleans_up_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let source = create(&dir.path().join("source.redb"));
    source
        .put("notes", "note", RecordId::new_v7(), &"value", 1)
        .unwrap();
    let backup_path = dir.path().join("backup.redb");
    source.backup(&backup_path).unwrap();

    let empty_target = dir.path().join("empty.redb");
    std::fs::write(&empty_target, b"").unwrap();
    let restored = Engine::restore(&backup_path, options(&empty_target), PASSWORD).unwrap();
    assert_eq!(restored.stats().unwrap().format_version, 1);
    drop(restored);

    let wrong_target = dir.path().join("wrong.redb");
    assert!(matches!(
        Engine::restore(&backup_path, options(&wrong_target), b"wrong password"),
        Err(Error::InvalidKey)
    ));
    assert!(!wrong_target.exists());

    let corrupt_path = dir.path().join("corrupt.redb");
    let corrupt_target = dir.path().join("corrupt-target.redb");
    std::fs::write(&corrupt_path, b"not a redb database").unwrap();
    assert!(Engine::restore(&corrupt_path, options(&corrupt_target), PASSWORD).is_err());
    assert!(!corrupt_target.exists());
}

#[test]
fn backup_during_online_writes_stays_readable() {
    let dir = tempfile::tempdir().unwrap();
    let source = Arc::new(create(&dir.path().join("source.redb")));
    let id = RecordId::new_v7();
    source
        .put("notes", "note", id, &"before-backup", 1)
        .unwrap();
    let backup_path = dir.path().join("backup.redb");
    let barrier = Arc::new(Barrier::new(3));

    let writer_engine = Arc::clone(&source);
    let writer_barrier = Arc::clone(&barrier);
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        for index in 0..300 {
            writer_engine
                .put(
                    "notes",
                    "note",
                    RecordId::new_v7(),
                    &format!("online-{index}"),
                    1,
                )
                .unwrap();
        }
    });

    let backup_engine = Arc::clone(&source);
    let backup_barrier = Arc::clone(&barrier);
    let backup_path_for_thread = backup_path.clone();
    let backup_thread = std::thread::spawn(move || {
        backup_barrier.wait();
        backup_engine.backup(&backup_path_for_thread).unwrap()
    });

    barrier.wait();
    let backup = backup_thread.join().unwrap();
    writer.join().unwrap();
    assert!(backup.row_count >= 1);

    let target_path = dir.path().join("restored.redb");
    let restored = Engine::restore(&backup_path, options(&target_path), PASSWORD).unwrap();
    assert_eq!(
        restored
            .get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "before-backup"
    );
}
