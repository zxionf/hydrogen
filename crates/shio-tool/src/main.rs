//! shio 明文导出/导入命令行工具。
//!
//! **危险**：`export` 写出的文件是明文；`import` 会修改数据库。
//! 口令只从标准输入、`SHIO_PASSWORD` 或终端提示读取，不接受命令行参数。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, IsTerminal};
use std::path::PathBuf;

use shio::{
    Credential, ExportOptions, ImportMode, ImportOptions, Kernel, OpenOptions,
    DEFAULT_MAX_VALUE_SIZE,
};
use zeroize::Zeroizing;

const USAGE: &str = "\
shio-tool — shio 明文导出/导入工具（危险）

用法:
  shio-tool export <db> --out <file> [--namespace N] [--collection C] [--with-deleted] [--force]
  shio-tool import <db> <file> [--dry-run] [--replace] [--batch N] [--yes]
  shio-tool verify <file>
  shio-tool stats  <file>

口令（按优先级）:
  --password-stdin   从标准输入读一行
  SHIO_PASSWORD      环境变量
  终端提示           不回显

退出码: 0 成功, 1 运行失败, 2 用法错误";

/// 命令失败：用法错误与运行错误分开，便于设置退出码。
enum Failure {
    Usage(String),
    Failed(String),
}

impl From<shio::Error> for Failure {
    fn from(error: shio::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

fn main() {
    let code = match run(std::env::args().skip(1).collect()) {
        Ok(()) => 0,
        Err(Failure::Usage(message)) => {
            eprintln!("{message}\n\n{USAGE}");
            2
        }
        Err(Failure::Failed(message)) => {
            eprintln!("error: {message}");
            1
        }
    };
    std::process::exit(code);
}

fn run(args: Vec<String>) -> Result<(), Failure> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage("缺少子命令"));
    };
    let rest = args.get(1..).unwrap_or(&[]);
    match command {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        "-V" | "--version" | "version" => {
            println!(
                "shio-tool {} (kernel {})",
                env!("CARGO_PKG_VERSION"),
                shio::version()
            );
            Ok(())
        }
        "export" => export(rest),
        "import" => import(rest),
        "verify" => verify(rest),
        "stats" => stats(rest),
        other => Err(usage(format!("未知子命令: {other}"))),
    }
}

fn export(args: &[String]) -> Result<(), Failure> {
    let mut cursor = Cursor::new(args);
    let mut database: Option<String> = None;
    let mut out: Option<String> = None;
    let mut namespace = None;
    let mut collection = None;
    let mut include_deleted = false;
    let mut overwrite = false;
    let mut password_stdin = false;

    while let Some(arg) = cursor.next() {
        match arg {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--out" => out = Some(cursor.value("--out")?.to_owned()),
            "--namespace" => namespace = Some(cursor.value("--namespace")?.to_owned()),
            "--collection" => collection = Some(cursor.value("--collection")?.to_owned()),
            "--with-deleted" => include_deleted = true,
            "--force" => overwrite = true,
            "--password-stdin" => password_stdin = true,
            other if other.starts_with('-') => {
                return Err(usage(format!("未知选项: {other}")));
            }
            other => {
                if database.is_some() {
                    return Err(usage(format!("多余的参数: {other}")));
                }
                database = Some(other.to_owned());
            }
        }
    }

    let database = database.ok_or_else(|| usage("export 需要数据库路径"))?;
    let out = out.ok_or_else(|| usage("export 需要 --out <file>"))?;
    let password = read_password(password_stdin)?;
    eprintln!("warning: 导出文件是明文，包含全部记录值；用完请自行清理。");

    let runtime = runtime()?;
    let kernel = open_kernel(&runtime, &database, true, &password)?;
    let info = runtime.block_on(kernel.export_records(
        PathBuf::from(&out),
        ExportOptions {
            namespace,
            collection,
            include_deleted,
            overwrite,
        },
    ))?;
    runtime.block_on(kernel.close())?;
    println!(
        "exported {} records ({} deleted) in {} collections, {} bytes -> {}",
        info.records, info.deleted, info.collections, info.bytes, out
    );
    Ok(())
}

fn import(args: &[String]) -> Result<(), Failure> {
    let mut cursor = Cursor::new(args);
    let mut database: Option<String> = None;
    let mut file: Option<String> = None;
    let mut dry_run = false;
    let mut replace = false;
    let mut batch = None;
    let mut yes = false;
    let mut password_stdin = false;

    while let Some(arg) = cursor.next() {
        match arg {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--dry-run" => dry_run = true,
            "--replace" => replace = true,
            "--batch" => {
                let value = cursor.value("--batch")?;
                let parsed = value
                    .parse::<usize>()
                    .map_err(|_| usage("--batch 需要非负整数"))?;
                batch = Some(parsed);
            }
            "--yes" => yes = true,
            "--password-stdin" => password_stdin = true,
            other if other.starts_with('-') => {
                return Err(usage(format!("未知选项: {other}")));
            }
            other => {
                if database.is_none() {
                    database = Some(other.to_owned());
                } else if file.is_none() {
                    file = Some(other.to_owned());
                } else {
                    return Err(usage(format!("多余的参数: {other}")));
                }
            }
        }
    }

    let database = database.ok_or_else(|| usage("import 需要数据库路径"))?;
    let file = file.ok_or_else(|| usage("import 需要导出文件路径"))?;
    if !dry_run && !yes {
        return Err(usage(
            "导入会修改数据库：先跑 --dry-run 预览，确认后加 --yes",
        ));
    }
    let password = read_password(password_stdin)?;

    let runtime = runtime()?;
    let kernel = open_kernel(&runtime, &database, false, &password)?;
    let report = runtime.block_on(kernel.import_records(
        PathBuf::from(&file),
        ImportOptions {
            mode: if replace {
                ImportMode::Replace
            } else {
                ImportMode::Merge
            },
            dry_run,
            batch_size: batch.unwrap_or(shio::DEFAULT_IMPORT_BATCH),
        },
    ))?;
    runtime.block_on(kernel.close())?;
    println!(
        "{}: records={} applied={} skipped={} deleted={} collections={} bytes={}",
        if report.dry_run {
            "dry-run"
        } else {
            "imported"
        },
        report.records,
        report.applied,
        report.skipped,
        report.deleted,
        report.collections,
        report.bytes
    );
    Ok(())
}

fn verify(args: &[String]) -> Result<(), Failure> {
    let file = single_file(args, "verify")?;
    let Some(file) = file else {
        return Ok(());
    };
    let info = shio::verify_export(&file, DEFAULT_MAX_VALUE_SIZE)?;
    println!(
        "valid export: format={} records={} deleted={} collections={} bytes={}",
        info.format_version, info.records, info.deleted, info.collections, info.bytes
    );
    Ok(())
}

fn stats(args: &[String]) -> Result<(), Failure> {
    let file = single_file(args, "stats")?;
    let Some(file) = file else {
        return Ok(());
    };
    let info = shio::verify_export(&file, DEFAULT_MAX_VALUE_SIZE)?;

    let mut counts: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    let reader = BufReader::new(std::fs::File::open(&file)?);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("t").and_then(|tag| tag.as_str()) != Some("rec") {
            continue;
        }
        let namespace = value
            .get("ns")
            .and_then(|item| item.as_str())
            .unwrap_or_default();
        let collection = value
            .get("col")
            .and_then(|item| item.as_str())
            .unwrap_or_default();
        let deleted = value
            .get("del")
            .and_then(|item| item.as_bool())
            .unwrap_or(false);
        let entry = counts
            .entry((namespace.to_owned(), collection.to_owned()))
            .or_insert((0, 0));
        entry.0 += 1;
        if deleted {
            entry.1 += 1;
        }
    }

    println!(
        "format={} records={} deleted={} collections={} bytes={}",
        info.format_version, info.records, info.deleted, info.collections, info.bytes
    );
    for ((namespace, collection), (records, deleted)) in &counts {
        println!("{namespace}/{collection}: {records} records ({deleted} deleted)");
    }
    Ok(())
}

/// 只接受一个位置参数（文件路径），`--help` 返回 `None`。
fn single_file(args: &[String], command: &str) -> Result<Option<PathBuf>, Failure> {
    let mut cursor = Cursor::new(args);
    let mut file: Option<PathBuf> = None;
    while let Some(arg) = cursor.next() {
        match arg {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            other if other.starts_with('-') => {
                return Err(usage(format!("未知选项: {other}")));
            }
            other => {
                if file.is_some() {
                    return Err(usage(format!("{command} 只接受一个文件路径")));
                }
                file = Some(PathBuf::from(other));
            }
        }
    }
    let file = file.ok_or_else(|| usage(format!("{command} 需要导出文件路径")))?;
    Ok(Some(file))
}

fn open_kernel(
    runtime: &tokio::runtime::Runtime,
    database: &str,
    read_only: bool,
    password: &str,
) -> Result<Kernel, Failure> {
    let options = OpenOptions::new(PathBuf::from(database)).read_only(read_only);
    let credential = Credential::password(password);
    Ok(runtime.block_on(Kernel::open(options, credential))?)
}

fn read_password(from_stdin: bool) -> Result<Zeroizing<String>, Failure> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let value = line.trim_end_matches(['\r', '\n']);
        if value.is_empty() {
            return Err(usage("从标准输入读到的口令为空"));
        }
        return Ok(Zeroizing::new(value.to_owned()));
    }
    if let Ok(value) = std::env::var("SHIO_PASSWORD") {
        if value.is_empty() {
            return Err(usage("SHIO_PASSWORD 为空"));
        }
        return Ok(Zeroizing::new(value));
    }
    if std::io::stdin().is_terminal() {
        let value = rpassword::prompt_password("Password: ")
            .map_err(|error| Failure::Failed(error.to_string()))?;
        if value.is_empty() {
            return Err(usage("口令为空"));
        }
        return Ok(Zeroizing::new(value));
    }
    Err(usage("没有口令：用 --password-stdin 或设置 SHIO_PASSWORD"))
}

fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Failure::Failed(format!("无法创建异步运行时: {error}")))
}

fn usage(message: impl Into<String>) -> Failure {
    Failure::Usage(message.into())
}

/// 顺序读取命令行参数：`value` 消耗下一个 token。
struct Cursor<'a> {
    args: &'a [String],
    index: usize,
}

impl<'a> Cursor<'a> {
    fn new(args: &'a [String]) -> Self {
        Self { args, index: 0 }
    }

    fn next(&mut self) -> Option<&'a str> {
        let item = self.args.get(self.index)?.as_str();
        self.index += 1;
        Some(item)
    }

    fn value(&mut self, flag: &str) -> Result<&'a str, Failure> {
        self.next().ok_or_else(|| usage(format!("{flag} 缺少参数")))
    }
}
