//! `sync` feature 的 facade 端到端用例。

#![cfg(feature = "sync")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use shio::{open_with_pairing, Credential, Kernel, OpenOptions};
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
