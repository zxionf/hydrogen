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
