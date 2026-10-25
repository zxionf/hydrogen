//! `shio-tool` 的命令行端到端用例。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use shio::{Credential, Kernel, ListOptions, OpenOptions, RecordId};

const PASSWORD: &str = "correct horse battery staple";
const BIN: &str = env!("CARGO_BIN_EXE_shio-tool");

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn open_async(path: &Path, create: bool) -> Kernel {
    Kernel::open(
        OpenOptions::new(path).create(create),
        Credential::password(PASSWORD),
    )
    .await
    .unwrap()
}

/// 建一个含 `records` 条记录的库，返回最后一个 ID。
fn seed(path: &Path, records: usize, delete_last: bool) -> RecordId {
    let runtime = runtime();
    runtime.block_on(async {
        let kernel = open_async(path, true).await;
        let notes = kernel
            .namespace("vault")
            .unwrap()
            .collection::<serde_json::Value>("entries")
            .unwrap();
        let mut last = RecordId::new_v7();
        for index in 0..records {
            last = notes
                .insert(&serde_json::json!({ "n": index }))
                .await
                .unwrap()
                .0;
        }
        if delete_last {
            assert!(notes.delete(&last).await.unwrap());
        }
        kernel.close().await.unwrap();
        drop(notes);
        last
    })
}

fn empty_db(path: &Path) {
    runtime().block_on(async {
        let kernel = open_async(path, true).await;
        kernel.close().await.unwrap();
    });
}

fn count(path: &Path) -> usize {
    let runtime = runtime();
    runtime.block_on(async {
        let kernel = open_async(path, false).await;
        let notes = kernel
            .namespace("vault")
            .unwrap()
            .collection::<serde_json::Value>("entries")
            .unwrap();
        let page = notes.list(ListOptions::default()).await.unwrap();
        kernel.close().await.unwrap();
        page.items.len()
    })
}

fn tool(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("SHIO_PASSWORD", PASSWORD)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn errors(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn path_text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

#[test]
fn export_verify_stats_and_import() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    let live_file = directory.path().join("live.jsonl");
    let full_file = directory.path().join("full.jsonl");
    seed(&source, 4, true);
    empty_db(&target);

    let output = tool(&[
        "export",
        &path_text(&source),
        "--out",
        &path_text(&live_file),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(errors(&output).contains("明文"));
    assert!(live_file.exists());

    let output = tool(&["verify", &path_text(&live_file)]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(text(&output).contains("records=3"), "{}", text(&output));
    assert!(text(&output).contains("deleted=0"), "{}", text(&output));

    let output = tool(&[
        "export",
        &path_text(&source),
        "--out",
        &path_text(&full_file),
        "--with-deleted",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    let output = tool(&["verify", &path_text(&full_file)]);
    assert!(text(&output).contains("records=4"), "{}", text(&output));
    assert!(text(&output).contains("deleted=1"), "{}", text(&output));

    let output = tool(&["stats", &path_text(&full_file)]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(
        text(&output).contains("vault/entries: 4 records (1 deleted)"),
        "{}",
        text(&output)
    );

    // 没有 --yes：用法错误，库不变。
    let output = tool(&["import", &path_text(&target), &path_text(&full_file)]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(count(&target), 0);

    // dry-run：成功但不写库。
    let output = tool(&[
        "import",
        &path_text(&target),
        &path_text(&full_file),
        "--dry-run",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(text(&output).contains("applied=3"), "{}", text(&output));
    assert_eq!(count(&target), 0);

    // 真正导入：3 条存活记录，墓碑跳过。
    let output = tool(&[
        "import",
        &path_text(&target),
        &path_text(&full_file),
        "--yes",
        "--batch",
        "2",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(text(&output).contains("applied=3"), "{}", text(&output));
    assert!(text(&output).contains("skipped=1"), "{}", text(&output));
    assert_eq!(count(&target), 3);

    // 重复导入幂等。
    let output = tool(&[
        "import",
        &path_text(&target),
        &path_text(&full_file),
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(text(&output).contains("applied=0"), "{}", text(&output));
    assert_eq!(count(&target), 3);
}

#[test]
fn replace_batch_and_overwrite_guards() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    let file = directory.path().join("rows.jsonl");
    seed(&source, 3, false);
    empty_db(&target);
    assert_eq!(
        tool(&["export", &path_text(&source), "--out", &path_text(&file)])
            .status
            .code(),
        Some(0)
    );

    // 已存在且无 --force：失败且保留原文件。
    let output = tool(&["export", &path_text(&source), "--out", &path_text(&file)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        errors(&output).contains("already exists"),
        "{}",
        errors(&output)
    );

    let output = tool(&[
        "export",
        &path_text(&source),
        "--out",
        &path_text(&file),
        "--force",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));

    let output = tool(&[
        "import",
        &path_text(&target),
        &path_text(&file),
        "--yes",
        "--replace",
        "--batch",
        "1",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(text(&output).contains("applied=3"), "{}", text(&output));
    assert_eq!(count(&target), 3);
}

#[test]
fn password_stdin_and_failures() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let file = directory.path().join("rows.jsonl");
    seed(&source, 1, false);

    // 口令走标准输入，不读环境变量。
    let mut child = Command::new(BIN)
        .args([
            "export",
            &path_text(&source),
            "--out",
            &path_text(&file),
            "--password-stdin",
        ])
        .env_remove("SHIO_PASSWORD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{PASSWORD}\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", errors(&output));
    assert!(file.exists());

    // 错误口令。
    let output = Command::new(BIN)
        .args([
            "export",
            &path_text(&source),
            "--out",
            &path_text(&directory.path().join("bad.jsonl")),
        ])
        .env("SHIO_PASSWORD", "wrong password")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));

    // 未知命令与缺参数是用法错误。
    assert_eq!(tool(&["nope"]).status.code(), Some(2));
    assert_eq!(
        tool(&["export", &path_text(&source)]).status.code(),
        Some(2)
    );

    // 帮助与版本。
    let output = tool(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output).contains("用法"));
    let output = tool(&["--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output).contains("shio-tool"));
}

#[test]
fn verify_rejects_truncated_and_tampered_files() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let file = directory.path().join("rows.jsonl");
    seed(&source, 2, false);
    assert_eq!(
        tool(&["export", &path_text(&source), "--out", &path_text(&file)])
            .status
            .code(),
        Some(0)
    );

    let content = std::fs::read_to_string(&file).unwrap();
    let mut lines: Vec<&str> = content.lines().collect();
    lines.pop();
    let truncated = directory.path().join("truncated.jsonl");
    std::fs::write(&truncated, lines.join("\n") + "\n").unwrap();
    let output = tool(&["verify", &path_text(&truncated)]);
    assert_eq!(output.status.code(), Some(1));
    assert!(errors(&output).contains("corrupt"), "{}", errors(&output));

    let tampered = directory.path().join("tampered.jsonl");
    std::fs::write(&tampered, content.replace("\"del\":false", "\"del\":true")).unwrap();
    let output = tool(&["verify", &path_text(&tampered)]);
    assert_eq!(output.status.code(), Some(1));

    let output = tool(&["stats", &path_text(&truncated)]);
    assert_eq!(output.status.code(), Some(1));
}
