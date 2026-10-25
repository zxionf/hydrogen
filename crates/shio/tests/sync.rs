//! `sync` feature 的 facade 端到端用例。

#![cfg(feature = "sync")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use shio::{open_with_pairing, Credential, Error, Kernel, ListOptions, OpenOptions};
use shio_core::model::RecordId;
use zeroize::Zeroizing;

const PASSWORD: &str = "correct horse battery staple";

fn options(path: &std::path::Path) -> OpenOptions {
    OpenOptions::new(path).create(true)
}

#[tokio::test]
async fn facade_sync_and_pairing() {
    let dir = tempfile::tempdir().unwrap();
    let a = Kernel::open(
        options(&dir.path().join("a.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let sync_a = a.sync().unwrap();
    assert!(sync_a.enable_sync(None).unwrap().baseline_done);
    assert_eq!(
        a.sync_info().unwrap().device_id,
        sync_a.info().unwrap().device_id
    );

    let notes = a
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    let id = RecordId::new_v7();
    notes.put(id, &"hello".to_owned()).await.unwrap();

    let offer = a
        .create_pairing_offer(Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(offer.fingerprint.len(), 6);
    let b = open_with_pairing(
        options(&dir.path().join("b.redb")),
        Credential::password(PASSWORD),
        &offer,
        Zeroizing::new(offer.secret.clone()),
    )
    .await
    .unwrap();
    assert_eq!(b.sync_info().unwrap().db_id, a.sync_info().unwrap().db_id);

    let sync_b = b.sync().unwrap();
    let page = sync_a.changes_since(0, 1000).unwrap();
    let frame = sync_a.encode(&page).unwrap();
    sync_b.apply(&frame).unwrap();
    let notes_b = b
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    assert_eq!(
        notes_b.get(&id).await.unwrap().unwrap().value,
        "hello".to_owned()
    );

    let snapshot = sync_a.snapshot().unwrap();
    let _ = snapshot;

    a.close().await.unwrap();
    assert!(matches!(a.sync(), Err(shio::Error::Closed)));
    assert!(matches!(a.sync_info(), Err(shio::Error::Closed)));
    b.close().await.unwrap();
}

#[tokio::test]
async fn facade_maintenance_mode_steps_migration() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::open(
        options(&dir.path().join("a.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let sync = kernel.sync().unwrap();
    sync.enable_sync(None).unwrap();

    let notes = kernel
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    let mut ids = Vec::new();
    for index in 0..6 {
        let id = RecordId::new_v7();
        notes.put(id, &format!("value-{index}")).await.unwrap();
        ids.push(id);
    }
    let to = sync.begin_rotation().unwrap().to;

    kernel.begin_maintenance().await.unwrap();
    assert!(matches!(
        notes.put(RecordId::new_v7(), &"blocked".to_owned()).await,
        Err(Error::Busy)
    ));
    assert_eq!(notes.get(&ids[0]).await.unwrap().unwrap().value, "value-0");
    assert_eq!(
        notes
            .list(ListOptions::default())
            .await
            .unwrap()
            .items
            .len(),
        6
    );

    let mut steps = 0;
    loop {
        let step = kernel.migrate_step(to, 2).await.unwrap();
        assert!(step.scanned <= 2);
        assert!(step.scanned >= step.migrated);
        steps += 1;
        assert!(steps < 10, "step migration did not finish");
        if step.done {
            break;
        }
    }
    let outcome = kernel.end_maintenance().await.unwrap();
    assert!(outcome.done);

    notes
        .put(RecordId::new_v7(), &"after".to_owned())
        .await
        .unwrap();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            notes.get(id).await.unwrap().unwrap().value,
            format!("value-{index}")
        );
    }

    // 合作式调度：不进入维护模式，tick 之间普通写照常。
    let to = sync.begin_rotation().unwrap().to;
    let budget = Duration::from_nanos(1);
    let first = kernel.migrate_tick(to, budget, 2).await.unwrap();
    assert!(first.scanned >= 1 && first.scanned <= 2);
    let between = RecordId::new_v7();
    notes
        .put(between, &"between-ticks".to_owned())
        .await
        .unwrap();

    let mut ticks = 1;
    loop {
        let step = kernel.migrate_tick(to, budget, 2).await.unwrap();
        assert!(step.scanned <= 2);
        assert!(step.scanned >= step.migrated);
        ticks += 1;
        assert!(ticks < 50, "tick migration did not finish");
        if step.done {
            break;
        }
    }
    assert_eq!(
        notes.get(&between).await.unwrap().unwrap().value,
        "between-ticks"
    );
    assert_eq!(notes.get(&ids[0]).await.unwrap().unwrap().value, "value-0");

    kernel.close().await.unwrap();
}

#[tokio::test]
async fn facade_auto_compaction_follows_reported_progress() {
    let dir = tempfile::tempdir().unwrap();
    let a = Kernel::open(
        options(&dir.path().join("a.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let info_a = a.sync().unwrap().enable_sync(None).unwrap();
    let notes = a
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    for index in 0..4 {
        notes
            .put(RecordId::new_v7(), &format!("value-{index}"))
            .await
            .unwrap();
    }

    let offer = a
        .create_pairing_offer(Duration::from_secs(600))
        .await
        .unwrap();
    let b = open_with_pairing(
        options(&dir.path().join("b.redb")),
        Credential::password(PASSWORD),
        &offer,
        Zeroizing::new(offer.secret.clone()),
    )
    .await
    .unwrap();

    let sync_a = a.sync().unwrap();
    let sync_b = b.sync().unwrap();
    let page = sync_a.changes_since(0, 1000).unwrap();
    sync_b.apply(&sync_a.encode(&page).unwrap()).unwrap();

    // B 上报进度；A 自动裁剪到最新一条之前。
    let frame = b.sync_progress_frame(info_a.device_id).await.unwrap();
    let cursor = a.apply_sync_progress(frame).await.unwrap();
    assert_eq!(cursor, page.next);
    assert_eq!(a.compact_changes_auto().await.unwrap(), cursor - 1);
    assert_eq!(a.sync_info().unwrap().log_start, cursor);

    // 落后游标要求重同步；最新一条仍可读。
    assert!(matches!(
        sync_a.changes_since(cursor - 2, 100),
        Err(Error::SyncResyncRequired { .. })
    ));
    assert_eq!(
        sync_a.changes_since(cursor - 1, 100).unwrap().entries.len(),
        1
    );

    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn facade_clone_backup_changes_device_identity() {
    let dir = tempfile::tempdir().unwrap();
    let a = Kernel::open(
        options(&dir.path().join("a.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let info_a = a.sync().unwrap().enable_sync(None).unwrap();
    let notes = a
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    let id = RecordId::new_v7();
    notes.put(id, &"hello".to_owned()).await.unwrap();

    let backup = dir.path().join("a.backup");
    a.backup(backup.clone()).await.unwrap();

    let (clone, info) = shio::clone_backup(
        options(&dir.path().join("clone.redb")),
        Credential::password(PASSWORD),
        backup,
    )
    .await
    .unwrap();
    assert_eq!(info.db_id, Some(info_a.db_id));
    assert_eq!(info.previous_device_id, Some(info_a.device_id));
    let device = info.device_id.expect("clone device id");
    assert_ne!(device, info_a.device_id);
    let clone_info = clone.sync_info().unwrap();
    assert_eq!(clone_info.device_id, device);
    assert_eq!(clone_info.db_id, info_a.db_id);
    assert_eq!(clone_info.log_start, 1);

    let notes_clone = clone
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    assert_eq!(
        notes_clone.get(&id).await.unwrap().unwrap().value,
        "hello".to_owned()
    );

    a.close().await.unwrap();
    clone.close().await.unwrap();
}

#[tokio::test]
async fn facade_forget_device_clears_peer() {
    let dir = tempfile::tempdir().unwrap();
    let a = Kernel::open(
        options(&dir.path().join("a.redb")),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap();
    let info_a = a.sync().unwrap().enable_sync(None).unwrap();
    let notes = a
        .namespace("notes")
        .unwrap()
        .collection::<String>("note")
        .unwrap();
    notes
        .put(RecordId::new_v7(), &"hello".to_owned())
        .await
        .unwrap();

    let offer = a
        .create_pairing_offer(Duration::from_secs(600))
        .await
        .unwrap();
    let b = open_with_pairing(
        options(&dir.path().join("b.redb")),
        Credential::password(PASSWORD),
        &offer,
        Zeroizing::new(offer.secret.clone()),
    )
    .await
    .unwrap();
    let info_b = b.sync_info().unwrap();

    // B 拉 A 的日志并上报进度；A 拉 B 的基线认识 B。
    let page = a.sync().unwrap().changes_since(0, 1000).unwrap();
    let frame = a.sync().unwrap().encode(&page).unwrap();
    b.sync().unwrap().apply(&frame).unwrap();
    let frame = b.sync().unwrap().progress_frame(info_a.device_id).unwrap();
    assert!(a.apply_sync_progress(frame).await.unwrap() > 0);
    let page = b.sync().unwrap().changes_since(0, 1000).unwrap();
    let frame = b.sync().unwrap().encode(&page).unwrap();
    a.sync().unwrap().apply(&frame).unwrap();

    let report = a.forget_device(info_b.device_id).await.unwrap();
    assert_eq!(report.device, info_b.device_id);
    assert!(report.removed_peer_cursor);
    assert!(report.removed_current_key);
    assert!(report.removed_progress);
    assert_eq!(report.removed_acks, 0);

    let again = a.forget_device(info_b.device_id).await.unwrap();
    assert!(!again.removed_peer_cursor);
    assert!(matches!(
        a.forget_device(info_a.device_id).await,
        Err(Error::InvalidArgument(_))
    ));

    a.close().await.unwrap();
    b.close().await.unwrap();
}
