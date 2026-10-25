//! 逻辑记录导出/导入的端到端用例。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use shio_core::crypto::kdf::KdfParams;
use shio_core::model::{ListOptions, RecordId};
use shio_core::store::{
    Engine, EngineOptions, ExportOptions, ImportMode, ImportOptions, DEFAULT_IMPORT_BATCH,
};
use shio_core::Error;

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

fn pair(source: &Engine, path: &Path) -> Engine {
    let (frame, secret) = source
        .create_pairing_bundle_with_secret(Duration::from_secs(600))
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

fn export(engine: &Engine, path: &Path, options: &ExportOptions) -> shio_core::store::ExportInfo {
    engine.export_records(path, options).expect("export")
}

fn import(engine: &Engine, path: &Path, options: &ImportOptions) -> shio_core::store::ImportReport {
    engine.import_records(path, options).expect("import")
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("read export")
        .lines()
        .map(|line| line.to_owned())
        .collect()
}

fn write_text(directory: &Path, name: &str, text: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, text).expect("write fixture");
    path
}

#[test]
fn export_import_roundtrip_across_formats() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source.enable_sync(None).expect("enable sync");

    let mut ids = Vec::new();
    for index in 0..4 {
        let id = RecordId::new_v7();
        source
            .put("vault", "entries", id, &format!("值{index}"), 1)
            .expect("put");
        ids.push(id);
    }
    let other = RecordId::new_v7();
    source
        .put("vault", "other", other, &"other", 1)
        .expect("put");
    assert!(source.delete("vault", "entries", &ids[1]).expect("delete"));

    let file = directory.path().join("source.jsonl");
    let info = export(
        &source,
        &file,
        &ExportOptions {
            include_deleted: true,
            ..ExportOptions::default()
        },
    );
    assert_eq!(info.format_version, 2);
    assert_eq!(info.records, 5);
    assert_eq!(info.deleted, 1);
    assert_eq!(info.collections, 2);
    assert!(info.bytes > 0);
    assert_eq!(info.bytes, std::fs::metadata(&file).unwrap().len());

    let lines = read_lines(&file);
    assert_eq!(lines.len(), 1 + 2 + 5 + 1);
    assert!(lines[0].contains("\"t\":\"head\""));
    assert!(lines[0].contains("\"fmt\":2"));
    assert!(lines.last().unwrap().contains("\"t\":\"end\""));
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line).expect("export line is JSON");
    }

    let target = create(&directory.path().join("target.db"));
    let report = import(&target, &file, &ImportOptions::default());
    assert_eq!(report.records, 5);
    assert_eq!(report.applied, 4);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.deleted, 0);
    assert_eq!(report.collections, 2);
    assert!(!report.dry_run);

    for (index, id) in ids.iter().enumerate() {
        if index == 1 {
            assert!(target
                .get::<String>("vault", "entries", id)
                .unwrap()
                .is_none());
            continue;
        }
        let stored = target
            .get::<String>("vault", "entries", id)
            .unwrap()
            .expect("record");
        assert_eq!(stored.value, format!("值{index}"));
    }
    assert_eq!(
        target
            .get::<String>("vault", "other", &other)
            .unwrap()
            .unwrap()
            .value,
        "other"
    );

    // 重跑同一文件：内容与时间戳一致，全部跳过。
    let again = import(&target, &file, &ImportOptions::default());
    assert_eq!(again.records, 5);
    assert_eq!(again.applied, 0);
    assert_eq!(again.skipped, 5);

    // 不含墓碑的导出：4 条记录、0 条墓碑。
    let live = directory.path().join("live.jsonl");
    let live_info = export(&target, &live, &ExportOptions::default());
    assert_eq!(live_info.records, 4);
    assert_eq!(live_info.deleted, 0);
    assert_eq!(live_info.collections, 2);

    // 反向导入到 format=1 的新库。
    let fresh = create(&directory.path().join("fresh.db"));
    let report = import(&fresh, &live, &ImportOptions::default());
    assert_eq!(report.applied, 4);
    let fresh_file = directory.path().join("fresh.jsonl");
    export(&fresh, &fresh_file, &ExportOptions::default());
    assert!(read_lines(&fresh_file)[0].contains("\"fmt\":1"));
}

#[test]
fn import_merge_keeps_newer_local_and_replace_overrides() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source.enable_sync(None).expect("enable sync");
    let id = RecordId::new_v7();
    source.put("vault", "entries", id, &"imported", 1).unwrap();
    let file = directory.path().join("one.jsonl");
    export(&source, &file, &ExportOptions::default());

    let target = create(&directory.path().join("target.db"));
    assert_eq!(import(&target, &file, &ImportOptions::default()).applied, 1);

    std::thread::sleep(Duration::from_millis(5));
    let local = target
        .put("vault", "entries", id, &"local", 1)
        .expect("local write");

    let merged = import(&target, &file, &ImportOptions::default());
    assert_eq!(merged.applied, 0);
    assert_eq!(merged.skipped, 1);
    assert_eq!(
        target
            .get::<String>("vault", "entries", &id)
            .unwrap()
            .unwrap()
            .value,
        "local"
    );

    let replaced = import(
        &target,
        &file,
        &ImportOptions {
            mode: ImportMode::Replace,
            ..ImportOptions::default()
        },
    );
    assert_eq!(replaced.applied, 1);
    let stored = target
        .get::<String>("vault", "entries", &id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.value, "imported");
    assert!(stored.meta.rev > local.rev);
}

#[test]
fn import_into_synced_db_is_syncable() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source.enable_sync(None).expect("enable sync");
    let mut ids = Vec::new();
    for index in 0..3 {
        let id = RecordId::new_v7();
        source
            .put("vault", "entries", id, &format!("v{index}"), 1)
            .unwrap();
        ids.push(id);
    }
    let file = directory.path().join("three.jsonl");
    export(&source, &file, &ExportOptions::default());

    // B 与 A 同库同 key，但为空；导入后应把记录写进日志。
    let b = pair(&source, &directory.path().join("b.db"));
    let before = b.sync_info().unwrap().log_end;
    assert_eq!(import(&b, &file, &ImportOptions::default()).applied, 3);
    assert!(b.sync_info().unwrap().log_end > before);

    // 新设备只拉 B 的日志就能拿到导入的记录。
    let d = pair(&b, &directory.path().join("d.db"));
    let mut cursor = 0;
    drain(&b, &mut cursor, &d);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            d.get::<String>("vault", "entries", id)
                .unwrap()
                .unwrap()
                .value,
            format!("v{index}")
        );
    }
}

#[test]
fn import_rejects_broken_files_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source.enable_sync(None).expect("enable sync");
    let id = RecordId::new_v7();
    source.put("vault", "entries", id, &"one", 1).unwrap();
    let deleted = RecordId::new_v7();
    source.put("vault", "entries", deleted, &"two", 1).unwrap();
    source.delete("vault", "entries", &deleted).unwrap();

    let valid_path = directory.path().join("valid.jsonl");
    export(
        &source,
        &valid_path,
        &ExportOptions {
            include_deleted: true,
            ..ExportOptions::default()
        },
    );
    let valid = read_lines(&valid_path);
    let body_start = 1;
    let last = valid.len() - 1;

    let cases: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        (
            "header-only",
            format!("{}\n", valid[0]),
        ),
        (
            "truncated",
            valid[..last].join("\n") + "\n",
        ),
        (
            "after-footer",
            format!("{}\n{}\n", valid.join("\n"), valid[body_start]),
        ),
        (
            "bad-checksum",
            format!(
                "{}\n{}\n",
                valid[..last].join("\n"),
                valid[last].replace(
                    valid[last]
                        .split("\"sha\":\"")
                        .nth(1)
                        .expect("sha")
                        .trim_end_matches("\"}"),
                    "0000000000000000000000000000000000000000000000000000000000000000"
                )
            ),
        ),
        (
            "count-mismatch",
            format!(
                "{}\n{}\n",
                valid[..last].join("\n"),
                valid[last].replace("\"rec\":2", "\"rec\":3")
            ),
        ),
        (
            "bad-base64",
            format!(
                "{}\n{}\n",
                valid[0],
                "{\"t\":\"rec\",\"ns\":\"vault\",\"col\":\"entries\",\"id\":\"018f3c1e-9f0a-7c3e-8f5d-2b1a0c9d8e7f\",\"rev\":1,\"created\":1,\"updated\":1,\"dv\":1,\"del\":false,\"v64\":\"!!!\"}"
            ),
        ),
        (
            "tombstone-with-value",
            format!(
                "{}\n{}\n",
                valid[0],
                "{\"t\":\"rec\",\"ns\":\"vault\",\"col\":\"entries\",\"id\":\"018f3c1e-9f0a-7c3e-8f5d-2b1a0c9d8e7f\",\"rev\":1,\"created\":1,\"updated\":1,\"dv\":1,\"del\":true,\"v64\":\"AA==\"}"
            ),
        ),
        (
            "bad-version",
            format!("{}\n{}", valid[0].replace("\"v\":1", "\"v\":9"), valid[last]),
        ),
    ];

    let target = create(&directory.path().join("target.db"));
    for (name, text) in cases {
        let path = write_text(directory.path(), &format!("{name}.jsonl"), &text);
        let error = target
            .import_records(&path, &ImportOptions::default())
            .expect_err(name);
        assert!(matches!(error, Error::Corrupted(_)), "{name}: {error:?}");
        let page = target
            .list("vault", "entries", ListOptions::default())
            .unwrap();
        assert!(page.items.is_empty(), "{name} wrote data");
    }
}

#[test]
fn export_filters_and_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    for index in 0..2 {
        source
            .put("one", "keep", RecordId::new_v7(), &index, 1)
            .unwrap();
    }
    source
        .put("one", "drop", RecordId::new_v7(), &"x", 1)
        .unwrap();
    source
        .put("two", "keep", RecordId::new_v7(), &"y", 1)
        .unwrap();

    let file = directory.path().join("filtered.jsonl");
    let info = export(
        &source,
        &file,
        &ExportOptions {
            namespace: Some("one".to_owned()),
            collection: Some("keep".to_owned()),
            ..ExportOptions::default()
        },
    );
    assert_eq!(info.records, 2);
    assert_eq!(info.collections, 1);

    let target = create(&directory.path().join("target.db"));
    assert_eq!(import(&target, &file, &ImportOptions::default()).applied, 2);
    assert_eq!(target.collections("one").unwrap(), vec!["keep".to_owned()]);
    assert!(target.collections("two").unwrap().is_empty());

    assert!(matches!(
        source.export_records(
            &directory.path().join("bad.jsonl"),
            &ExportOptions {
                collection: Some("keep".to_owned()),
                ..ExportOptions::default()
            }
        ),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        source.export_records(&file, &ExportOptions::default()),
        Err(Error::InvalidArgument(_))
    ));
    let overwritten = source
        .export_records(
            &file,
            &ExportOptions {
                overwrite: true,
                ..ExportOptions::default()
            },
        )
        .unwrap();
    assert_eq!(overwritten.records, 4);
}

#[test]
fn read_only_export_and_import_guards() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.db");
    let writer = create(&path);
    writer
        .put("vault", "entries", RecordId::new_v7(), &"one", 1)
        .unwrap();
    drop(writer);

    let reader = Engine::open(options(&path).read_only(true), PASSWORD).unwrap();
    let file = directory.path().join("readonly.jsonl");
    let info = export(&reader, &file, &ExportOptions::default());
    assert_eq!(info.records, 1);

    let error = reader
        .import_records(&file, &ImportOptions::default())
        .unwrap_err();
    assert!(matches!(error, Error::ReadOnly));

    // dry-run 是只读的，只读句柄也能预演；自导入内容相同，全部跳过。
    let preview = reader
        .import_records(
            &file,
            &ImportOptions {
                dry_run: true,
                ..ImportOptions::default()
            },
        )
        .unwrap();
    assert!(preview.dry_run);
    assert_eq!(preview.records, 1);
    assert_eq!(preview.applied, 0);
    assert_eq!(preview.skipped, 1);
}

#[test]
fn dry_run_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    for index in 0..3 {
        source
            .put("vault", "entries", RecordId::new_v7(), &index, 1)
            .unwrap();
    }
    let file = directory.path().join("three.jsonl");
    export(&source, &file, &ExportOptions::default());

    let target = create(&directory.path().join("target.db"));
    let preview = import(
        &target,
        &file,
        &ImportOptions {
            dry_run: true,
            ..ImportOptions::default()
        },
    );
    assert_eq!(preview.applied, 3);
    assert_eq!(preview.skipped, 0);
    assert!(preview.dry_run);
    assert!(target
        .list("vault", "entries", ListOptions::default())
        .unwrap()
        .items
        .is_empty());

    let real = import(&target, &file, &ImportOptions::default());
    assert_eq!(real.applied, 3);
    assert_eq!(
        target
            .list("vault", "entries", ListOptions::default())
            .unwrap()
            .items
            .len(),
        3
    );
}

#[test]
fn format_one_roundtrip_and_binary_values() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("v1.db"));
    let binary = RecordId::new_v7();
    source
        .put_raw("bin", "blob", binary, &[0x00, 0x01, 0xfe, 0xff], 7)
        .unwrap();
    let text = RecordId::new_v7();
    source.put("bin", "blob", text, &"hello", 1).unwrap();
    let removed = RecordId::new_v7();
    source.put("bin", "blob", removed, &"gone", 1).unwrap();
    source.delete("bin", "blob", &removed).unwrap();

    let file = directory.path().join("v1.jsonl");
    let info = export(
        &source,
        &file,
        &ExportOptions {
            include_deleted: true,
            ..ExportOptions::default()
        },
    );
    assert_eq!(info.format_version, 1);
    assert!(read_lines(&file)[0].contains("\"fmt\":1"));

    let target = create(&directory.path().join("v1-copy.db"));
    let report = import(&target, &file, &ImportOptions::default());
    assert_eq!(report.applied, 2);
    assert_eq!(report.skipped, 1);
    let (meta, value) = target.get_raw("bin", "blob", &binary).unwrap().unwrap();
    assert_eq!(value.as_slice(), &[0x00, 0x01, 0xfe, 0xff]);
    assert_eq!(meta.data_version, 7);
    assert_eq!(target.stats().unwrap().format_version, 1);
}

#[test]
fn import_rejects_values_over_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source
        .put("vault", "entries", RecordId::new_v7(), &"x".repeat(128), 1)
        .unwrap();
    let file = directory.path().join("big.jsonl");
    export(&source, &file, &ExportOptions::default());

    let target = Engine::open(
        options(&directory.path().join("small.db"))
            .create(true)
            .max_value_size(16),
        PASSWORD,
    )
    .unwrap();
    let error = target
        .import_records(&file, &ImportOptions::default())
        .unwrap_err();
    assert!(matches!(error, Error::ValueTooLarge { .. }), "{error:?}");
    assert!(target
        .list("vault", "entries", ListOptions::default())
        .unwrap()
        .items
        .is_empty());
}

#[test]
fn import_batch_size_is_validated() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source
        .put("vault", "entries", RecordId::new_v7(), &"one", 1)
        .unwrap();
    let file = directory.path().join("one.jsonl");
    export(&source, &file, &ExportOptions::default());

    let target = create(&directory.path().join("target.db"));
    let error = target
        .import_records(
            &file,
            &ImportOptions {
                batch_size: shio_core::store::MAX_IMPORT_BATCH + 1,
                ..ImportOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::InvalidArgument(_)));

    let report = import(
        &target,
        &file,
        &ImportOptions {
            batch_size: 0,
            ..ImportOptions::default()
        },
    );
    assert_eq!(report.applied, 1);
    assert_eq!(DEFAULT_IMPORT_BATCH, 256);
}

#[test]
fn open_engine_can_import_exported_file() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.db");
    let source = create(&source_path);
    let id = RecordId::new_v7();
    source.put("vault", "entries", id, &"persisted", 1).unwrap();
    let file = directory.path().join("persisted.jsonl");
    export(&source, &file, &ExportOptions::default());
    drop(source);

    let target_path = directory.path().join("target.db");
    {
        let target = create(&target_path);
        import(&target, &file, &ImportOptions::default());
    }
    let reopened = open(&target_path);
    assert_eq!(
        reopened
            .get::<String>("vault", "entries", &id)
            .unwrap()
            .unwrap()
            .value,
        "persisted"
    );
}

#[test]
fn verify_export_checks_files_without_a_database() {
    let directory = tempfile::tempdir().unwrap();
    let source = create(&directory.path().join("source.db"));
    source
        .put("vault", "entries", RecordId::new_v7(), &"one", 1)
        .unwrap();
    let file = directory.path().join("one.jsonl");
    let info = export(&source, &file, &ExportOptions::default());

    let verified = shio_core::store::verify_export(&file, shio_core::model::DEFAULT_MAX_VALUE_SIZE)
        .expect("verify export");
    assert_eq!(verified, info);

    let text = std::fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let truncated = directory.path().join("truncated.jsonl");
    std::fs::write(&truncated, lines[..lines.len() - 1].join("\n") + "\n").unwrap();
    assert!(matches!(
        shio_core::store::verify_export(&truncated, shio_core::model::DEFAULT_MAX_VALUE_SIZE),
        Err(Error::Corrupted(_))
    ));

    assert!(matches!(
        shio_core::store::verify_export(&file, 1),
        Err(Error::ValueTooLarge { .. })
    ));
    assert!(matches!(
        shio_core::store::verify_export(&directory.path().join("missing.jsonl"), 1024),
        Err(Error::NotFound) | Err(Error::Io(_))
    ));
}
