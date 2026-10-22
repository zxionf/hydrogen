//! 最小多端同步示例：配对、拉取一页变更、在另一设备应用。
//!
//! 运行：`cargo run -p shio --features sync --example sync`

use std::fs;
use std::time::Duration;

use shio::{open_with_pairing, Credential, Kernel, OpenOptions};
use shio_core::model::RecordId;
use zeroize::Zeroizing;

const PASSWORD: &str = "correct horse battery staple";

#[tokio::main]
async fn main() -> shio::Result<()> {
    let dir = std::env::temp_dir().join(format!("shio-sync-example-{}", std::process::id()));
    fs::create_dir_all(&dir)?;

    let a = Kernel::open(
        OpenOptions::new(dir.join("a.redb")).create(true),
        Credential::password(PASSWORD),
    )
    .await?;
    let sync_a = a.sync()?;
    sync_a.enable_sync(None)?;

    let notes = a.namespace("notes")?.collection::<String>("note")?;
    let id = RecordId::new_v7();
    notes.put(id, &"hello from A".to_owned()).await?;

    let offer = a.create_pairing_offer(Duration::from_secs(600)).await?;
    println!("pairing fingerprint: {}", offer.fingerprint);

    let b = open_with_pairing(
        OpenOptions::new(dir.join("b.redb")).create(true),
        Credential::password(PASSWORD),
        &offer,
        Zeroizing::new(offer.secret.clone()),
    )
    .await?;
    let sync_b = b.sync()?;

    let page = sync_a.changes_since(0, 1000)?;
    let frame = sync_a.encode(&page)?;
    sync_b.apply(&frame)?;

    let notes_b = b.namespace("notes")?.collection::<String>("note")?;
    let value = notes_b.get(&id).await?.map(|stored| stored.value);
    println!("B read: {value:?}");

    a.close().await?;
    b.close().await?;
    fs::remove_dir_all(&dir)?;
    Ok(())
}
