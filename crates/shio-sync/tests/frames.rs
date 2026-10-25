//! `SHY2` 帧与同步句柄的端到端用例。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::time::Duration;

use shio_core::crypto::kdf::KdfParams;
use shio_core::model::RecordId;
use shio_core::store::{Engine, EngineOptions};
use shio_sync::{decode_pairing_secret, frame, Error, Sync};

const PASSWORD: &[u8] = b"correct horse battery staple";

fn fast_kdf() -> KdfParams {
    KdfParams::new(8_192, 1, 1)
}

fn options(path: &Path) -> EngineOptions {
    EngineOptions::new(path).kdf(fast_kdf())
}

fn create(path: &Path) -> Engine {
    Engine::open(options(path).create(true), PASSWORD).unwrap()
}

#[test]
fn change_offer_and_snapshot_frames_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    a.enable_sync(None).unwrap();
    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"hello", 1).unwrap();

    let sync_a = Sync::new(&a);
    let offer = sync_a.pairing_offer(Duration::from_secs(600)).unwrap();
    let secret = decode_pairing_secret(&offer.secret).unwrap();
    assert_eq!(offer.fingerprint.len(), 6);
    let b = Sync::accept_pairing(
        options(&dir.path().join("b.redb")),
        PASSWORD,
        &offer,
        &secret,
    )
    .unwrap();
    let sync_b = Sync::new(&b);

    let page = sync_a.changes_since(0, 1000).unwrap();
    let frame = sync_a.encode(&page).unwrap();
    let decoded = sync_b.decode(&frame).unwrap();
    assert_eq!(decoded, page);
    sync_b.apply(&frame).unwrap();
    assert_eq!(
        b.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "hello"
    );

    a.rotate_data_key().unwrap();
    let offers = sync_a.offers().unwrap();
    let frame = sync_a.encode_offers(&offers).unwrap();
    let decoded = sync_b.decode_offers(&frame).unwrap();
    assert_eq!(decoded, offers);
    sync_b.apply_offers(&frame).unwrap();
    assert_eq!(
        b.sync_info().unwrap().current_key_id,
        a.sync_info().unwrap().current_key_id
    );

    let mut iter = sync_a.snapshot().unwrap();
    let mut seen = 0;
    while let Some(page) = iter.next_page(2).unwrap() {
        let frame = sync_a.encode_snapshot(&page).unwrap();
        let decoded = sync_b.decode_snapshot(&frame).unwrap();
        assert_eq!(decoded, page);
        sync_b.apply_snapshot(&decoded).unwrap();
        seen += page.entries.len();
    }
    assert!(seen >= 1);
    assert_eq!(
        b.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "hello"
    );
}

#[test]
fn db_id_mismatch_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    a.enable_sync(None).unwrap();
    a.put("notes", "note", RecordId::new_v7(), &"a", 1).unwrap();
    let b = create(&dir.path().join("b.redb"));
    b.enable_sync(None).unwrap();

    let sync_a = Sync::new(&a);
    let sync_b = Sync::new(&b);
    let frame = sync_a
        .encode(&sync_a.changes_since(0, 100).unwrap())
        .unwrap();
    assert!(sync_b.decode(&frame).is_err());
}

#[test]
fn pairing_accepts_empty_file_and_cleans_failed_target() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    a.enable_sync(None).unwrap();
    let sync_a = Sync::new(&a);
    let offer = sync_a.pairing_offer(Duration::from_secs(600)).unwrap();
    let secret = decode_pairing_secret(&offer.secret).unwrap();

    let empty = dir.path().join("empty.redb");
    std::fs::File::create(&empty).unwrap();
    let b = Sync::accept_pairing(options(&empty), PASSWORD, &offer, &secret).unwrap();
    assert!(b.sync_info().unwrap().baseline_done);
    drop(b);

    let failed = dir.path().join("failed.redb");
    assert!(Sync::accept_pairing(options(&failed), PASSWORD, &offer, &[0u8; 32]).is_err());
    assert!(!failed.exists());
}

#[test]
fn owned_handle_keeps_engine_alive() {
    let dir = tempfile::tempdir().unwrap();
    let engine = create(&dir.path().join("a.redb"));
    engine.enable_sync(None).unwrap();
    let handle = shio_sync::SyncHandle::new(std::sync::Arc::new(engine));
    let info = handle.info().unwrap();
    assert!(info.baseline_done);
    let mut snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.at_seq(), info.log_end);
    assert!(snapshot.next_page(10).unwrap().is_none());
}

#[test]
fn progress_frames_roundtrip_and_drive_auto_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let a = create(&dir.path().join("a.redb"));
    let info_a = a.enable_sync(None).unwrap();
    for index in 0..6 {
        a.put("notes", "note", RecordId::new_v7(), &index, 1)
            .unwrap();
    }
    let sync_a = Sync::new(&a);

    // 配对出 B，并让 B 读完 A 当时的日志。
    let offer = sync_a.pairing_offer(Duration::from_secs(600)).unwrap();
    let secret = decode_pairing_secret(&offer.secret).unwrap();
    let b = Sync::accept_pairing(
        options(&dir.path().join("b.redb")),
        PASSWORD,
        &offer,
        &secret,
    )
    .unwrap();
    let sync_b = Sync::new(&b);
    let page = sync_a.changes_since(0, 1000).unwrap();
    let change_frame = sync_a.encode(&page).unwrap();
    sync_b.apply(&change_frame).unwrap();

    // B 把「我读到 page.next」送回 A；A 据此裁剪。
    let progress = sync_b.progress_frame(info_a.device_id).unwrap();
    assert_eq!(progress.len(), frame::HEADER_LEN);
    let cursor = sync_a.apply_progress(&progress).unwrap();
    assert_eq!(cursor, page.next);
    // 保留最新一条，保证新变更的序列号继续递增。
    assert_eq!(sync_a.compact_changes_auto().unwrap(), cursor - 1);
    assert_eq!(sync_a.info().unwrap().log_start, cursor);

    // 进度帧的事务往返与负例校验。
    assert!(matches!(
        frame::decode_progress([9u8; 16], &progress),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        frame::decode_progress(info_a.db_id, &change_frame),
        Err(Error::InvalidArgument(_))
    ));

    // 落后于裁剪点的拉取会明确要求重同步；最新一条仍可读。
    assert!(matches!(
        sync_a.changes_since(cursor - 2, 100),
        Err(Error::SyncResyncRequired { .. })
    ));
    assert_eq!(
        sync_a.changes_since(cursor - 1, 100).unwrap().entries.len(),
        1
    );

    // 已消费游标继续增量：A 新写一条，B 能拉到。
    let id = RecordId::new_v7();
    a.put("notes", "note", id, &"after-compact", 1).unwrap();
    let page = sync_a.changes_since(cursor, 1000).unwrap();
    assert_eq!(page.entries.len(), 1);
    sync_b.apply(&sync_a.encode(&page).unwrap()).unwrap();
    assert_eq!(
        b.get::<String>("notes", "note", &id)
            .unwrap()
            .unwrap()
            .value,
        "after-compact"
    );
}
