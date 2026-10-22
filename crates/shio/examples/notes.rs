//! 最小使用示例：创建临时库、写入、读取、列出、删除，然后关闭。
//!
//! 运行：`cargo run -p shio --example notes`

use serde::{Deserialize, Serialize};
use shio::{Credential, Kernel, ListOptions, OpenOptions};

#[derive(Debug, Serialize, Deserialize)]
struct Note {
    title: String,
    body: String,
}

#[tokio::main]
async fn main() -> shio::Result<()> {
    let path = std::env::temp_dir().join(format!("shio-example-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let kernel = Kernel::open(
        OpenOptions::new(&path).create(true),
        Credential::password("correct horse battery staple"),
    )
    .await?;

    let notes = kernel.namespace("notes")?.collection::<Note>("note")?;
    let (id, meta) = notes
        .insert(&Note {
            title: "第一篇".to_owned(),
            body: "hello shio".to_owned(),
        })
        .await?;
    println!("inserted {id} rev={}", meta.rev);

    if let Some(stored) = notes.get(&id).await? {
        println!("{} / {}", stored.value.title, stored.value.body);
    }

    let page = notes.list(ListOptions::default()).await?;
    println!("{} notes, next={:?}", page.items.len(), page.next);

    notes.delete(&id).await?;
    println!("after delete: {:?}", notes.get(&id).await?);

    kernel.close().await?;
    let _ = std::fs::remove_file(&path);
    Ok(())
}
