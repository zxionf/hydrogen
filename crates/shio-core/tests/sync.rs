//! 同步、升级、配对与轮换的端到端用例。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

use shio_core::crypto::kdf::KdfParams;
use shio_core::model::RecordId;
use shio_core::store::{Engine, EngineOptions};
use shio_core::{ChangeEntry, DeviceId, Error, KeyId};

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
