//! C ABI 同步冒烟测试：配对、变更帧、快照帧与轮换只走公开符号。
//!
//! Kotlin 不解析 SHY2 帧；本测试也只把帧当不透明字节搬运。

#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use serde_json::Value;
use shio_ffi::*;

fn last_error() -> String {
    let mut buffer = vec![0 as c_char; 1024];
    // SAFETY: 缓冲区长度与指针匹配；句柄允许为 NULL。
    let required = unsafe { shio_last_error(ptr::null(), buffer.as_mut_ptr(), buffer.len()) };
    if required == 0 {
        return String::new();
    }
    // SAFETY: shio_last_error 在缓冲区足够时补 NUL。
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn take_buffer(pointer: *mut u8, len: usize) -> Vec<u8> {
    if pointer.is_null() {
        assert_eq!(len, 0);
        return Vec::new();
    }
    // SAFETY: 指针和长度来自一次成功的 shio 输出。
    let bytes = unsafe { std::slice::from_raw_parts(pointer, len) }.to_vec();
    // SAFETY: 同一个缓冲区只释放一次。
    unsafe { shio_free(pointer, len) };
    bytes
}

fn parse(bytes: Vec<u8>) -> Value {
    serde_json::from_slice(&bytes).expect("shio returned invalid JSON")
}

fn open(path: &Path, password: &[u8], options: &[u8]) -> *mut ShioKernel {
    let path = CString::new(path.to_str().unwrap()).unwrap();
    let mut handle = ptr::null_mut();
    // SAFETY: 所有指针都指向有效缓冲区，输出句柄可写。
    let status = unsafe {
        shio_open(
            path.as_ptr(),
            password.as_ptr(),
            password.len(),
            options.as_ptr(),
            options.len(),
            &mut handle,
        )
    };
    assert_eq!(status, SHIO_OK, "open failed: {}", last_error());
    assert!(!handle.is_null());
    handle
}

fn sync_info(handle: *mut ShioKernel) -> Value {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_info(handle, &mut pointer, &mut len) };
    assert_eq!(status, SHIO_OK, "sync_info failed: {}", last_error());
    parse(take_buffer(pointer, len))
}

fn migration_progress(handle: *mut ShioKernel) -> Value {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_migration_progress(handle, &mut pointer, &mut len) };
    assert_eq!(
        status,
        SHIO_OK,
        "migration_progress failed: {}",
        last_error()
    );
    parse(take_buffer(pointer, len))
}

fn enable_sync(handle: *mut ShioKernel) -> Value {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄与输出指针有效；device_id 为 NULL 时自动生成。
    let status = unsafe { shio_sync_enable(handle, ptr::null(), &mut pointer, &mut len) };
    assert_eq!(status, SHIO_OK, "enable_sync failed: {}", last_error());
    parse(take_buffer(pointer, len))
}

fn changes(handle: *mut ShioKernel, after: u64) -> (Vec<u8>, u64, bool) {
    let mut frame = ptr::null_mut();
    let mut frame_len = 0;
    let mut next = 0;
    let mut has_more = 0;
    // SAFETY: 句柄与所有输出指针有效。
    let status = unsafe {
        shio_sync_changes(
            handle,
            after,
            100,
            &mut frame,
            &mut frame_len,
            &mut next,
            &mut has_more,
        )
    };
    assert_eq!(status, SHIO_OK, "changes failed: {}", last_error());
    (take_buffer(frame, frame_len), next, has_more != 0)
}

fn apply(handle: *mut ShioKernel, frame: &[u8]) -> Value {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄、帧与输出指针有效。
    let status =
        unsafe { shio_sync_apply(handle, frame.as_ptr(), frame.len(), &mut pointer, &mut len) };
    assert_eq!(status, SHIO_OK, "apply failed: {}", last_error());
    parse(take_buffer(pointer, len))
}

fn offers(handle: *mut ShioKernel) -> Vec<u8> {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_offers(handle, &mut pointer, &mut len) };
    assert_eq!(status, SHIO_OK, "offers failed: {}", last_error());
    take_buffer(pointer, len)
}

fn apply_offers(handle: *mut ShioKernel, frame: &[u8]) {
    // SAFETY: 句柄与帧有效。
    let status = unsafe { shio_sync_apply_offers(handle, frame.as_ptr(), frame.len()) };
    assert_eq!(status, SHIO_OK, "apply_offers failed: {}", last_error());
}

fn snapshot_page(
    handle: *mut ShioKernel,
    after: Option<&[u8]>,
) -> (Vec<u8>, Option<Vec<u8>>, bool) {
    let mut frame = ptr::null_mut();
    let mut frame_len = 0;
    let mut next = ptr::null_mut();
    let mut next_len = 0;
    let mut done = 0;
    let (after_pointer, after_len) = match after {
        Some(bytes) => (bytes.as_ptr(), bytes.len()),
        None => (ptr::null(), 0),
    };
    // SAFETY: 句柄、可选 after 与所有输出指针有效。
    let status = unsafe {
        shio_sync_snapshot_page(
            handle,
            after_pointer,
            after_len,
            100,
            &mut frame,
            &mut frame_len,
            &mut next,
            &mut next_len,
            &mut done,
        )
    };
    assert_eq!(status, SHIO_OK, "snapshot_page failed: {}", last_error());
    let next = if next.is_null() {
        None
    } else {
        Some(take_buffer(next, next_len))
    };
    (take_buffer(frame, frame_len), next, done != 0)
}

fn apply_snapshot(handle: *mut ShioKernel, frame: &[u8]) -> Value {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄、帧与输出指针有效。
    let status = unsafe {
        shio_sync_apply_snapshot(handle, frame.as_ptr(), frame.len(), &mut pointer, &mut len)
    };
    assert_eq!(status, SHIO_OK, "apply_snapshot failed: {}", last_error());
    parse(take_buffer(pointer, len))
}

fn create_pairing(handle: *mut ShioKernel) -> (Vec<u8>, String) {
    let mut frame = ptr::null_mut();
    let mut frame_len = 0;
    let mut json = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe {
        shio_sync_create_pairing(
            handle,
            0,
            &mut frame,
            &mut frame_len,
            &mut json,
            &mut json_len,
        )
    };
    assert_eq!(status, SHIO_OK, "create_pairing failed: {}", last_error());
    let metadata = parse(take_buffer(json, json_len));
    assert_eq!(metadata["secret"].as_str().unwrap().len(), 52);
    assert_eq!(metadata["fingerprint"].as_str().unwrap().len(), 6);
    (
        take_buffer(frame, frame_len),
        metadata["secret"].as_str().unwrap().to_owned(),
    )
}

fn try_open_with_pairing(
    path: &Path,
    password: &[u8],
    frame: &[u8],
    secret: &str,
) -> (i32, *mut ShioKernel) {
    let path = CString::new(path.to_str().unwrap()).unwrap();
    let options = br#"{"create":true}"#;
    let mut handle = ptr::null_mut();
    // SAFETY: 所有输入指针有效，输出句柄可写。
    let status = unsafe {
        shio_open_with_pairing(
            path.as_ptr(),
            password.as_ptr(),
            password.len(),
            options.as_ptr(),
            options.len(),
            frame.as_ptr(),
            frame.len(),
            secret.as_ptr(),
            secret.len(),
            &mut handle,
        )
    };
    (status, handle)
}

fn open_with_pairing(path: &Path, password: &[u8], frame: &[u8], secret: &str) -> *mut ShioKernel {
    let (status, handle) = try_open_with_pairing(path, password, frame, secret);
    assert_eq!(
        status,
        SHIO_OK,
        "open_with_pairing failed: {}",
        last_error()
    );
    assert!(!handle.is_null());
    handle
}

fn uuid_bytes(text: &str) -> [u8; 16] {
    let compact: String = text.chars().filter(|character| *character != '-').collect();
    assert_eq!(compact.len(), 32);
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16).unwrap();
    }
    bytes
}

fn hex_bytes(text: &str) -> [u8; 16] {
    assert_eq!(text.len(), 32);
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
    }
    bytes
}

fn insert(
    handle: *mut ShioKernel,
    namespace: &CString,
    collection: &CString,
    value: &[u8],
) -> Value {
    let mut id = [0u8; 16];
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄、名称、载荷与输出指针有效。
    let status = unsafe {
        shio_insert(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            value.as_ptr(),
            value.len(),
            id.as_mut_ptr(),
            &mut pointer,
            &mut len,
        )
    };
    assert_eq!(status, SHIO_OK, "insert failed: {}", last_error());
    parse(take_buffer(pointer, len))
}

fn get(handle: *mut ShioKernel, namespace: &CString, collection: &CString, id: &str) -> Value {
    let id = uuid_bytes(id);
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄、名称、ID 与输出指针有效。
    let status = unsafe {
        shio_get(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            id.as_ptr(),
            &mut pointer,
            &mut len,
        )
    };
    assert_eq!(status, SHIO_OK, "get failed: {}", last_error());
    parse(take_buffer(pointer, len))
}

#[test]
fn sync_pair_changes_snapshot_and_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let path_a = directory.path().join("a.db");
    let path_b = directory.path().join("b.db");
    let create = br#"{"create":true}"#;

    let a = open(&path_a, b"device-a-password", create);

    // 未启用同步时状态查询必须拒绝。
    let mut info_pointer = ptr::null_mut();
    let mut info_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_info(a, &mut info_pointer, &mut info_len) };
    assert_eq!(status, SHIO_SYNC_DISABLED);
    assert!(info_pointer.is_null());
    // 维护模式要求先启用同步。
    // SAFETY: 句柄有效。
    assert_eq!(
        unsafe { shio_sync_begin_maintenance(a) },
        SHIO_SYNC_DISABLED
    );

    let info_a = enable_sync(a);
    assert_eq!(info_a["device_id"].as_str().unwrap().len(), 32);
    assert_eq!(info_a["db_id"].as_str().unwrap().len(), 32);

    let mut stats_pointer = ptr::null_mut();
    let mut stats_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_stats(a, &mut stats_pointer, &mut stats_len) };
    assert_eq!(status, SHIO_OK, "stats failed: {}", last_error());
    let stats = parse(take_buffer(stats_pointer, stats_len));
    assert_eq!(stats["format_version"], 2);

    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    let meta = insert(a, &namespace, &collection, br#"{"title":"from-a"}"#);
    let id_a = meta["id"].as_str().unwrap().to_owned();

    // 同步启用后禁止物理删除。
    let mut purged = -1;
    // SAFETY: 句柄、名称、ID 与输出指针有效。
    let status = unsafe {
        shio_purge(
            a,
            namespace.as_ptr(),
            collection.as_ptr(),
            uuid_bytes(&id_a).as_ptr(),
            &mut purged,
        )
    };
    assert_eq!(status, SHIO_SYNC_ENABLED);
    assert_eq!(purged, 0);

    let (pairing_frame, secret) = create_pairing(a);
    let mut wrong_secret = secret.clone().into_bytes();
    wrong_secret[0] = if wrong_secret[0] == b'0' { b'1' } else { b'0' };
    let wrong_secret = String::from_utf8(wrong_secret).unwrap();
    let wrong_path = directory.path().join("wrong.db");
    let (status, wrong_handle) = try_open_with_pairing(
        &wrong_path,
        b"wrong-password",
        &pairing_frame,
        &wrong_secret,
    );
    assert_eq!(status, SHIO_PAIRING_INVALID);
    assert!(wrong_handle.is_null());

    let b = open_with_pairing(&path_b, b"device-b-password", &pairing_frame, &secret);
    let info_b = sync_info(b);
    assert_eq!(info_b["db_id"], info_a["db_id"]);

    // 配对目标已有内容时必须拒绝，不能覆盖已有库。
    let (status, duplicate_handle) =
        try_open_with_pairing(&path_b, b"device-b-password", &pairing_frame, &secret);
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(duplicate_handle.is_null());

    // A → B 变更帧。
    let (change_frame, next, has_more) = changes(a, 0);
    assert!(!change_frame.is_empty());
    assert!(next > 0);
    assert!(!has_more);
    let report = apply(b, &change_frame);
    assert!(report["applied"].as_u64().unwrap() >= 1);
    assert_eq!(
        get(b, &namespace, &collection, &id_a)["value"]["title"],
        "from-a"
    );

    // B → A 变更帧，验证双向应用。
    let meta_b = insert(b, &namespace, &collection, br#"{"title":"from-b"}"#);
    let id_b = meta_b["id"].as_str().unwrap().to_owned();
    let (back_frame, _, _) = changes(b, 0);
    apply(a, &back_frame);
    assert_eq!(
        get(a, &namespace, &collection, &id_b)["value"]["title"],
        "from-b"
    );

    // 快照帧可以被另一端应用。
    let mut at_seq = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_snapshot_start(a, &mut at_seq) };
    assert_eq!(status, SHIO_OK, "snapshot_start failed: {}", last_error());
    assert!(at_seq >= 1);
    let (snapshot_frame, snapshot_next, done) = snapshot_page(a, None);
    assert!(!snapshot_frame.is_empty());
    assert!(done);
    assert!(snapshot_next.is_none());
    apply_snapshot(b, &snapshot_frame);

    // 轮换：A 产生 offer，B 安装；A 迁移后两端仍能读。
    let mut rotation_pointer = ptr::null_mut();
    let mut rotation_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_begin_rotation(a, &mut rotation_pointer, &mut rotation_len) };
    assert_eq!(status, SHIO_OK, "begin_rotation failed: {}", last_error());
    let rotation = parse(take_buffer(rotation_pointer, rotation_len));
    let to = rotation["to"].as_str().unwrap().to_owned();

    let initial_progress = migration_progress(a);
    assert_eq!(initial_progress["active"], false);
    assert_eq!(initial_progress["done"], false);
    assert_eq!(initial_progress["migrated"], 0);
    // 没有迁移时取消是安全的；随后的迁移会清除取消标志。
    // SAFETY: 句柄有效。
    let status = unsafe { shio_sync_cancel_migration(a) };
    assert_eq!(status, SHIO_OK, "cancel_migration failed: {}", last_error());

    let to_bytes = hex_bytes(&to);
    // SAFETY: 句柄与 key ID 有效。
    let status = unsafe { shio_sync_migrate_all_ex(a, to_bytes.as_ptr(), 1, 0) };
    assert_eq!(status, SHIO_OK, "migrate_all_ex failed: {}", last_error());
    let progress = migration_progress(a);
    assert_eq!(progress["active"], false);
    assert_eq!(progress["done"], true);
    assert!(progress["migrated"].as_u64().unwrap() >= 1);
    assert!(progress["scanned"].as_u64().unwrap() >= progress["migrated"].as_u64().unwrap());

    // 旧入口保持可用；已迁移的记录返回 0。
    let mut migrated = 1;
    // SAFETY: 句柄、key ID 与输出指针有效。
    let status = unsafe { shio_sync_migrate_all(a, to_bytes.as_ptr(), &mut migrated) };
    assert_eq!(status, SHIO_OK, "migrate_all failed: {}", last_error());
    assert_eq!(migrated, 0);

    // B 还没安装新 key；直接应用迁移后的快照必须映射为 ROTATION_REQUIRED。
    let (locked_snapshot, _, _) = snapshot_page(a, None);
    let mut locked_report = ptr::null_mut();
    let mut locked_report_len = 0;
    // SAFETY: 句柄、帧与输出指针有效。
    let status = unsafe {
        shio_sync_apply_snapshot(
            b,
            locked_snapshot.as_ptr(),
            locked_snapshot.len(),
            &mut locked_report,
            &mut locked_report_len,
        )
    };
    assert_eq!(status, SHIO_ROTATION_REQUIRED);
    assert!(locked_report.is_null());

    // 安装 offer 后同一份快照可以应用。
    apply_offers(b, &offers(a));
    let (unlocked_snapshot, _, _) = snapshot_page(a, None);
    apply_snapshot(b, &unlocked_snapshot);

    let (rotated_frame, _, _) = changes(a, 0);
    apply(b, &rotated_frame);
    assert_eq!(
        get(b, &namespace, &collection, &id_a)["value"]["title"],
        "from-a"
    );

    // 显式 ack 是幂等的补救入口；安装 offer 时通常已经自动 ack。
    // SAFETY: 句柄与 key ID 有效。
    let status = unsafe { shio_sync_ack_rotation(b, hex_bytes(&to).as_ptr()) };
    assert_eq!(status, SHIO_OK, "ack_rotation failed: {}", last_error());

    // 日志被压缩后，从旧游标拉取必须要求走快照。
    let info_after = sync_info(a);
    let log_end = info_after["log_end"].as_u64().unwrap();
    let mut removed = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_compact(a, log_end, &mut removed) };
    assert_eq!(status, SHIO_OK, "compact failed: {}", last_error());
    assert!(removed > 0);
    let mut compacted_frame = ptr::null_mut();
    let mut compacted_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe {
        shio_sync_changes(
            a,
            0,
            100,
            &mut compacted_frame,
            &mut compacted_len,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    assert_eq!(status, SHIO_SYNC_RESYNC_REQUIRED);
    assert!(compacted_frame.is_null());

    // SAFETY: 句柄有效。
    unsafe { shio_close(a) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(b) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(a) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(b) };
}

#[test]
fn sync_migration_progress_cancel_and_resume() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("a.db");
    let create = br#"{"create":true}"#;
    let a = open(&path, b"device-a-password", create);
    enable_sync(a);
    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    for index in 0..40 {
        let value = format!(r#"{{"index":{index}}}"#);
        insert(a, &namespace, &collection, value.as_bytes());
    }

    let mut rotation_pointer = ptr::null_mut();
    let mut rotation_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_begin_rotation(a, &mut rotation_pointer, &mut rotation_len) };
    assert_eq!(status, SHIO_OK, "begin_rotation failed: {}", last_error());
    let rotation = parse(take_buffer(rotation_pointer, rotation_len));
    let to = hex_bytes(rotation["to"].as_str().unwrap());

    let handle_address = a as usize;
    let worker_to = to;
    let worker = std::thread::spawn(move || {
        // SAFETY: 主线程在 join 之前保持句柄有效；只并发调用线程安全入口。
        unsafe {
            shio_sync_migrate_all_ex(handle_address as *mut ShioKernel, worker_to.as_ptr(), 1, 20)
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = migration_progress(a);
        if progress["active"].as_bool().unwrap() && progress["migrated"].as_u64().unwrap() >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "migration did not make progress: {progress}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // SAFETY: 句柄有效。
    let status = unsafe { shio_sync_cancel_migration(a) };
    assert_eq!(status, SHIO_OK, "cancel_migration failed: {}", last_error());
    assert_eq!(worker.join().unwrap(), SHIO_CANCELLED);
    let progress = migration_progress(a);
    assert_eq!(progress["active"], false);
    assert_eq!(progress["done"], false);
    assert!(progress["migrated"].as_u64().unwrap() >= 1);

    // SAFETY: 句柄与 key ID 有效。
    let status = unsafe { shio_sync_migrate_all_ex(a, to.as_ptr(), 0, 0) };
    assert_eq!(status, SHIO_OK, "resume failed: {}", last_error());
    let progress = migration_progress(a);
    assert_eq!(progress["active"], false);
    assert_eq!(progress["done"], true);
    assert!(progress["migrated"].as_u64().unwrap() >= 1);
    assert!(progress["scanned"].as_u64().unwrap() >= progress["migrated"].as_u64().unwrap());

    // 非法参数在启动迁移前被拒绝。
    // SAFETY: 句柄与 key ID 有效。
    let status = unsafe { shio_sync_migrate_all_ex(a, to.as_ptr(), 0, 60_001) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);

    // 维护模式：期间写被拒绝，步进每次只处理一批，退出报告是否仍有游标。
    let mut rotation_pointer = ptr::null_mut();
    let mut rotation_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_begin_rotation(a, &mut rotation_pointer, &mut rotation_len) };
    assert_eq!(status, SHIO_OK, "begin_rotation failed: {}", last_error());
    let rotation = parse(take_buffer(rotation_pointer, rotation_len));
    let to2 = hex_bytes(rotation["to"].as_str().unwrap());

    // SAFETY: 句柄有效。
    let status = unsafe { shio_sync_begin_maintenance(a) };
    assert_eq!(
        status,
        SHIO_OK,
        "begin_maintenance failed: {}",
        last_error()
    );

    let mut id = [0u8; 16];
    let mut meta_ptr = ptr::null_mut();
    let mut meta_len = 0;
    let payload = br#"{"blocked":true}"#;
    // SAFETY: 句柄、名称、载荷与输出指针有效。
    let status = unsafe {
        shio_insert(
            a,
            namespace.as_ptr(),
            collection.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            id.as_mut_ptr(),
            &mut meta_ptr,
            &mut meta_len,
        )
    };
    assert_eq!(status, SHIO_BUSY);
    assert!(meta_ptr.is_null());

    // 读操作不受维护模式影响。
    let mut collections_ptr = ptr::null_mut();
    let mut collections_len = 0;
    // SAFETY: 句柄、名称与输出指针有效。
    let status = unsafe {
        shio_collections(
            a,
            namespace.as_ptr(),
            &mut collections_ptr,
            &mut collections_len,
        )
    };
    assert_eq!(status, SHIO_OK, "collections failed: {}", last_error());
    assert_eq!(
        parse(take_buffer(collections_ptr, collections_len)),
        serde_json::json!(["entries"])
    );

    // 单步迁移：每步 7 条，直到 done。
    let mut steps = 0;
    let mut step_done = false;
    while !step_done {
        let mut json_ptr = ptr::null_mut();
        let mut json_len = 0;
        // SAFETY: 句柄、key ID 与输出指针有效。
        let status =
            unsafe { shio_sync_migrate_step(a, to2.as_ptr(), 7, &mut json_ptr, &mut json_len) };
        assert_eq!(status, SHIO_OK, "migrate_step failed: {}", last_error());
        let step = parse(take_buffer(json_ptr, json_len));
        let scanned = step["scanned"].as_u64().unwrap();
        assert!(scanned <= 7);
        assert!(scanned >= step["migrated"].as_u64().unwrap());
        step_done = step["done"].as_bool().unwrap();
        steps += 1;
        assert!(steps < 20, "step migration did not finish");
    }
    assert_eq!(migration_progress(a)["done"], true);

    let mut done = 0u8;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_end_maintenance(a, &mut done) };
    assert_eq!(status, SHIO_OK, "end_maintenance failed: {}", last_error());
    assert_eq!(done, 1);

    // 退出后普通写恢复。
    let mut meta_ptr = ptr::null_mut();
    let mut meta_len = 0;
    // SAFETY: 句柄、名称、载荷与输出指针有效。
    let status = unsafe {
        shio_insert(
            a,
            namespace.as_ptr(),
            collection.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            id.as_mut_ptr(),
            &mut meta_ptr,
            &mut meta_len,
        )
    };
    assert_eq!(
        status,
        SHIO_OK,
        "insert after maintenance failed: {}",
        last_error()
    );
    assert!(!take_buffer(meta_ptr, meta_len).is_empty());

    // 步进参数越界与空 key ID 在写事务前被拒绝。
    let mut json_ptr = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、key ID 与输出指针有效。
    let status =
        unsafe { shio_sync_migrate_step(a, to2.as_ptr(), 2048, &mut json_ptr, &mut json_len) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    // SAFETY: NULL key ID 应被拒绝。
    let status = unsafe { shio_sync_migrate_step(a, ptr::null(), 1, &mut json_ptr, &mut json_len) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);

    // 合作式调度：不进入维护模式，tick 之间普通写照常。
    let mut rotation_pointer = ptr::null_mut();
    let mut rotation_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_begin_rotation(a, &mut rotation_pointer, &mut rotation_len) };
    assert_eq!(status, SHIO_OK, "begin_rotation failed: {}", last_error());
    let rotation = parse(take_buffer(rotation_pointer, rotation_len));
    let to3 = hex_bytes(rotation["to"].as_str().unwrap());

    let mut json_ptr = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、key ID 与输出指针有效。
    let status =
        unsafe { shio_sync_migrate_tick(a, to3.as_ptr(), 1, 4, &mut json_ptr, &mut json_len) };
    assert_eq!(status, SHIO_OK, "migrate_tick failed: {}", last_error());
    let tick = parse(take_buffer(json_ptr, json_len));
    assert!(tick["scanned"].as_u64().unwrap() >= 1);

    let mut tick_id = [0u8; 16];
    let mut meta_ptr = ptr::null_mut();
    let mut meta_len = 0;
    let tick_payload = br#"{"between":"ticks"}"#;
    // SAFETY: 句柄、名称、载荷与输出指针有效。
    let status = unsafe {
        shio_insert(
            a,
            namespace.as_ptr(),
            collection.as_ptr(),
            tick_payload.as_ptr(),
            tick_payload.len(),
            tick_id.as_mut_ptr(),
            &mut meta_ptr,
            &mut meta_len,
        )
    };
    assert_eq!(
        status,
        SHIO_OK,
        "insert between ticks failed: {}",
        last_error()
    );
    assert!(!take_buffer(meta_ptr, meta_len).is_empty());

    let mut ticks = 1;
    let mut tick_done = false;
    while !tick_done {
        let mut json_ptr = ptr::null_mut();
        let mut json_len = 0;
        // SAFETY: 句柄、key ID 与输出指针有效。
        let status =
            unsafe { shio_sync_migrate_tick(a, to3.as_ptr(), 1, 4, &mut json_ptr, &mut json_len) };
        assert_eq!(status, SHIO_OK, "migrate_tick failed: {}", last_error());
        let tick = parse(take_buffer(json_ptr, json_len));
        assert!(tick["scanned"].as_u64().unwrap() >= tick["migrated"].as_u64().unwrap());
        tick_done = tick["done"].as_bool().unwrap();
        ticks += 1;
        assert!(ticks < 60, "tick migration did not finish");
    }
    assert_eq!(migration_progress(a)["done"], true);
    assert_eq!(migration_progress(a)["active"], false);

    // tick 参数越界与空 key ID 同样在写事务前被拒绝。
    let mut json_ptr = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、key ID 与输出指针有效。
    let status =
        unsafe { shio_sync_migrate_tick(a, to3.as_ptr(), 1_001, 4, &mut json_ptr, &mut json_len) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    // SAFETY: 句柄、key ID 与输出指针有效。
    let status =
        unsafe { shio_sync_migrate_tick(a, to3.as_ptr(), 10, 257, &mut json_ptr, &mut json_len) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    // SAFETY: NULL key ID 应被拒绝。
    let status =
        unsafe { shio_sync_migrate_tick(a, ptr::null(), 10, 4, &mut json_ptr, &mut json_len) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);

    // SAFETY: 句柄有效。
    unsafe { shio_close(a) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(a) };
}

#[test]
fn backup_and_restore_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.db");
    let backup_path = directory.path().join("backup.db");
    let target_path = directory.path().join("target.db");
    let wrong_target_path = directory.path().join("wrong-target.db");
    let create = br#"{"create":true}"#;

    let source = open(&source_path, b"backup-password", create);
    enable_sync(source);
    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    let id = insert(source, &namespace, &collection, br#"{"name":"backup"}"#);
    let id_text = id["id"].as_str().unwrap().to_owned();
    let before = sync_info(source);

    let backup_c = CString::new(backup_path.to_str().unwrap()).unwrap();
    let mut json_pointer = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、路径与输出指针有效。
    let status =
        unsafe { shio_backup(source, backup_c.as_ptr(), &mut json_pointer, &mut json_len) };
    assert_eq!(status, SHIO_OK, "backup failed: {}", last_error());
    let summary = parse(take_buffer(json_pointer, json_len));
    assert_eq!(summary["format_version"], 2);
    assert!(summary["row_count"].as_u64().unwrap() >= 1);

    // 目标已存在时拒绝覆盖。
    let mut ignored_pointer = ptr::null_mut();
    let mut ignored_len = 0;
    // SAFETY: 句柄、路径与输出指针有效。
    let status = unsafe {
        shio_backup(
            source,
            backup_c.as_ptr(),
            &mut ignored_pointer,
            &mut ignored_len,
        )
    };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(ignored_pointer.is_null());

    let target_c = CString::new(target_path.to_str().unwrap()).unwrap();
    let mut restored = ptr::null_mut();
    // SAFETY: 路径、口令、options 与输出指针有效。
    let status = unsafe {
        shio_restore(
            backup_c.as_ptr(),
            target_c.as_ptr(),
            b"backup-password".as_ptr(),
            b"backup-password".len(),
            ptr::null(),
            0,
            &mut restored,
        )
    };
    assert_eq!(status, SHIO_OK, "restore failed: {}", last_error());
    assert!(!restored.is_null());

    let after = sync_info(restored);
    assert_eq!(after["db_id"], before["db_id"]);
    assert_eq!(after["device_id"], before["device_id"]);
    assert_eq!(after["current_key_id"], before["current_key_id"]);
    let value = get(restored, &namespace, &collection, &id_text);
    assert_eq!(value["value"]["name"], "backup");

    // 错误口令不得发布目标。
    let wrong_target_c = CString::new(wrong_target_path.to_str().unwrap()).unwrap();
    let mut wrong_handle = ptr::null_mut();
    // SAFETY: 路径、口令、options 与输出指针有效。
    let status = unsafe {
        shio_restore(
            backup_c.as_ptr(),
            wrong_target_c.as_ptr(),
            b"wrong-password".as_ptr(),
            b"wrong-password".len(),
            ptr::null(),
            0,
            &mut wrong_handle,
        )
    };
    assert_eq!(status, SHIO_INVALID_KEY);
    assert!(wrong_handle.is_null());
    assert!(!wrong_target_path.exists());

    // SAFETY: 句柄有效。
    unsafe { shio_close(source) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(restored) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(source) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(restored) };
}

#[test]
fn sync_rejects_foreign_database_frame() {
    let directory = tempfile::tempdir().unwrap();
    let path_a = directory.path().join("a.db");
    let path_c = directory.path().join("c.db");
    let create = br#"{"create":true}"#;

    let a = open(&path_a, b"device-a-password", create);
    enable_sync(a);
    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    insert(a, &namespace, &collection, br#"{"title":"only-a"}"#);
    let (foreign_frame, _, _) = changes(a, 0);

    let c = open(&path_c, b"device-c-password", create);
    enable_sync(c);
    let mut report = ptr::null_mut();
    let mut report_len = 0;
    // SAFETY: 句柄、帧与输出指针有效。
    let status = unsafe {
        shio_sync_apply(
            c,
            foreign_frame.as_ptr(),
            foreign_frame.len(),
            &mut report,
            &mut report_len,
        )
    };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(report.is_null());

    // SAFETY: 句柄有效。
    unsafe { shio_close(a) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(c) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(a) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(c) };
}

fn progress_frame(handle: *mut ShioKernel, peer: &[u8; 16]) -> Vec<u8> {
    let mut frame = ptr::null_mut();
    let mut frame_len = 0;
    // SAFETY: 句柄、设备 ID 与输出指针有效。
    let status =
        unsafe { shio_sync_progress_frame(handle, peer.as_ptr(), &mut frame, &mut frame_len) };
    assert_eq!(status, SHIO_OK, "progress_frame failed: {}", last_error());
    take_buffer(frame, frame_len)
}

fn apply_progress(handle: *mut ShioKernel, frame: &[u8]) -> u64 {
    let mut cursor = 0;
    // SAFETY: 句柄、帧与输出指针有效。
    let status =
        unsafe { shio_sync_apply_progress(handle, frame.as_ptr(), frame.len(), &mut cursor) };
    assert_eq!(status, SHIO_OK, "apply_progress failed: {}", last_error());
    cursor
}

fn compact_auto(handle: *mut ShioKernel) -> u64 {
    let mut count = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_sync_compact_auto(handle, &mut count) };
    assert_eq!(status, SHIO_OK, "compact_auto failed: {}", last_error());
    count
}

#[test]
fn progress_report_drives_auto_log_compaction() {
    let directory = tempfile::tempdir().unwrap();
    let create = br#"{"create":true}"#;
    let a = open(&directory.path().join("a.db"), b"auto-compact", create);
    let info_a = enable_sync(a);
    let device_a = uuid_bytes(info_a["device_id"].as_str().unwrap());

    let (frame, secret) = create_pairing(a);
    let b = open_with_pairing(
        &directory.path().join("b.db"),
        b"auto-compact",
        &frame,
        &secret,
    );

    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    for index in 0..5u8 {
        let id = [index + 1; 16];
        let payload = format!(r#"{{"n":{index}}}"#);
        let mut buffer = ptr::null_mut();
        let mut len = 0;
        // SAFETY: 句柄与输入输出指针有效。
        let status = unsafe {
            shio_put(
                a,
                namespace.as_ptr(),
                collection.as_ptr(),
                id.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                &mut buffer,
                &mut len,
            )
        };
        assert_eq!(status, SHIO_OK, "put failed: {}", last_error());
        assert!(!take_buffer(buffer, len).is_empty());
    }

    let (change_frame, next, _) = changes(a, 0);
    apply(b, &change_frame);

    // B 上报「我读到 next」；A 据此只删到最新一条之前。
    let progress = progress_frame(b, &device_a);
    let cursor = apply_progress(a, &progress);
    assert_eq!(cursor, next);
    assert_eq!(compact_auto(a), cursor - 1);
    let info = sync_info(a);
    assert_eq!(info["log_start"].as_u64().unwrap(), cursor);
    assert_eq!(info["log_end"].as_u64().unwrap(), cursor);

    // 落后游标要求重新走快照。
    let mut frame_ptr = ptr::null_mut();
    let mut frame_len = 0;
    let mut out_next = 0;
    let mut out_more = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe {
        shio_sync_changes(
            a,
            cursor - 2,
            100,
            &mut frame_ptr,
            &mut frame_len,
            &mut out_next,
            &mut out_more,
        )
    };
    assert_eq!(status, SHIO_SYNC_RESYNC_REQUIRED);
    assert!(frame_ptr.is_null() && frame_len == 0);

    // 未启用同步的库拒绝自动裁剪。
    let plain = open(&directory.path().join("plain.db"), b"auto-compact", create);
    assert_eq!(
        unsafe { shio_sync_compact_auto(plain, ptr::null_mut()) },
        SHIO_SYNC_DISABLED
    );
    // SAFETY: 句柄有效且没有其它线程使用。
    unsafe { shio_close(plain) };
    // SAFETY: 同上。
    unsafe { shio_kernel_destroy(plain) };

    // SAFETY: 句柄有效且没有其它线程使用。
    unsafe { shio_close(a) };
    // SAFETY: 同上。
    unsafe { shio_kernel_destroy(a) };
    // SAFETY: 同上。
    unsafe { shio_close(b) };
    // SAFETY: 同上。
    unsafe { shio_kernel_destroy(b) };
}

#[test]
fn clone_backup_rotates_device_identity() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.db");
    let backup_path = directory.path().join("backup.db");
    let clone_path = directory.path().join("clone.db");
    let occupied_path = directory.path().join("occupied.db");
    let create = br#"{"create":true}"#;

    let source = open(&source_path, b"clone-password", create);
    enable_sync(source);
    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    let id = insert(source, &namespace, &collection, br#"{"name":"clone"}"#);
    let id_text = id["id"].as_str().unwrap().to_owned();
    let before = sync_info(source);

    let backup_c = CString::new(backup_path.to_str().unwrap()).unwrap();
    let mut json_pointer = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、路径与输出指针有效。
    let status =
        unsafe { shio_backup(source, backup_c.as_ptr(), &mut json_pointer, &mut json_len) };
    assert_eq!(status, SHIO_OK, "backup failed: {}", last_error());
    take_buffer(json_pointer, json_len);

    let clone_c = CString::new(clone_path.to_str().unwrap()).unwrap();
    let mut clone = ptr::null_mut();
    let mut clone_json = ptr::null_mut();
    let mut clone_json_len = 0;
    // SAFETY: 路径、口令、options 与输出指针有效。
    let status = unsafe {
        shio_clone_backup(
            backup_c.as_ptr(),
            clone_c.as_ptr(),
            b"clone-password".as_ptr(),
            b"clone-password".len(),
            ptr::null(),
            0,
            &mut clone,
            &mut clone_json,
            &mut clone_json_len,
        )
    };
    assert_eq!(status, SHIO_OK, "clone failed: {}", last_error());
    assert!(!clone.is_null());
    let summary = parse(take_buffer(clone_json, clone_json_len));
    assert_eq!(summary["format_version"], 2);
    assert_eq!(summary["db_id"], before["db_id"]);
    assert_eq!(summary["previous_device_id"], before["device_id"]);
    assert_ne!(summary["device_id"], before["device_id"]);
    assert_eq!(summary["peer_count"], 0);
    assert_eq!(summary["log_entries"], 2);

    let after = sync_info(clone);
    assert_eq!(after["db_id"], before["db_id"]);
    assert_eq!(after["device_id"], summary["device_id"]);
    assert_eq!(after["current_key_id"], before["current_key_id"]);
    assert_eq!(after["log_start"], 1);
    assert_eq!(after["log_end"], 2);
    assert_eq!(after["baseline_done"], true);
    let value = get(clone, &namespace, &collection, &id_text);
    assert_eq!(value["value"]["name"], "clone");

    // 目标非空时拒绝，且不发布句柄或摘要。
    std::fs::write(&occupied_path, b"occupied").unwrap();
    let occupied_c = CString::new(occupied_path.to_str().unwrap()).unwrap();
    let mut ignored = ptr::null_mut();
    let mut ignored_json = ptr::null_mut();
    let mut ignored_len = 0;
    // SAFETY: 路径、口令、options 与输出指针有效。
    let status = unsafe {
        shio_clone_backup(
            backup_c.as_ptr(),
            occupied_c.as_ptr(),
            b"clone-password".as_ptr(),
            b"clone-password".len(),
            ptr::null(),
            0,
            &mut ignored,
            &mut ignored_json,
            &mut ignored_len,
        )
    };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(ignored.is_null());
    assert!(ignored_json.is_null());

    // SAFETY: 句柄有效。
    unsafe { shio_close(source) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(clone) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(source) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(clone) };
}

fn list_len(handle: *mut ShioKernel, namespace: &CString, collection: &CString) -> usize {
    let mut pointer = ptr::null_mut();
    let mut len = 0;
    // SAFETY: 句柄、名称与输出指针有效。
    let status = unsafe {
        shio_list(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            ptr::null(),
            100,
            0,
            &mut pointer,
            &mut len,
        )
    };
    assert_eq!(status, SHIO_OK, "list failed: {}", last_error());
    let page = parse(take_buffer(pointer, len));
    page["items"].as_array().unwrap().len()
}

#[test]
fn export_and_import_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.db");
    let target_path = directory.path().join("target.db");
    let preview_path = directory.path().join("preview.db");
    let third_path = directory.path().join("third.db");
    let export_path = directory.path().join("rows.jsonl");
    let truncated_path = directory.path().join("truncated.jsonl");
    let create = br#"{"create":true}"#;
    let namespace = CString::new("vault").unwrap();
    let collection = CString::new("entries").unwrap();
    let password = b"transfer-password";

    let source = open(&source_path, password, create);
    let kept = insert(source, &namespace, &collection, br#"{"name":"kept"}"#);
    let kept_id = kept["id"].as_str().unwrap().to_owned();
    let removed = insert(source, &namespace, &collection, br#"{"name":"removed"}"#);
    let removed_id = removed["id"].as_str().unwrap().to_owned();
    let removed_bytes = uuid_bytes(&removed_id);
    // SAFETY: 句柄、名称与 ID 有效。
    let status = unsafe {
        shio_delete(
            source,
            namespace.as_ptr(),
            collection.as_ptr(),
            removed_bytes.as_ptr(),
            ptr::null_mut(),
        )
    };
    assert_eq!(status, SHIO_OK, "delete failed: {}", last_error());

    let export_c = CString::new(export_path.to_str().unwrap()).unwrap();
    let options = br#"{"include_deleted":true}"#;
    let mut json_pointer = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、路径、options 与输出指针有效。
    let status = unsafe {
        shio_export(
            source,
            export_c.as_ptr(),
            options.as_ptr(),
            options.len(),
            &mut json_pointer,
            &mut json_len,
        )
    };
    assert_eq!(status, SHIO_OK, "export failed: {}", last_error());
    let summary = parse(take_buffer(json_pointer, json_len));
    assert_eq!(summary["format_version"], 1);
    assert_eq!(summary["records"], 2);
    assert_eq!(summary["deleted"], 1);
    assert_eq!(summary["collections"], 1);
    assert!(summary["bytes"].as_u64().unwrap() > 0);

    let target = open(&target_path, password, create);
    let import_options = br#"{"batch_size":1}"#;
    let mut json_pointer = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、路径、options 与输出指针有效。
    let status = unsafe {
        shio_import(
            target,
            export_c.as_ptr(),
            import_options.as_ptr(),
            import_options.len(),
            &mut json_pointer,
            &mut json_len,
        )
    };
    assert_eq!(status, SHIO_OK, "import failed: {}", last_error());
    let report = parse(take_buffer(json_pointer, json_len));
    assert_eq!(report["records"], 2);
    assert_eq!(report["applied"], 1);
    assert_eq!(report["skipped"], 1);
    assert_eq!(report["dry_run"], false);
    assert_eq!(report["collections"], 1);
    let value = get(target, &namespace, &collection, &kept_id);
    assert_eq!(value["value"]["name"], "kept");
    assert_eq!(list_len(target, &namespace, &collection), 1);

    // dry-run：只预演，不写入。
    let preview = open(&preview_path, password, create);
    let dry = br#"{"dry_run":true}"#;
    let mut json_pointer = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、路径、options 与输出指针有效。
    let status = unsafe {
        shio_import(
            preview,
            export_c.as_ptr(),
            dry.as_ptr(),
            dry.len(),
            &mut json_pointer,
            &mut json_len,
        )
    };
    assert_eq!(status, SHIO_OK, "dry-run failed: {}", last_error());
    let preview_report = parse(take_buffer(json_pointer, json_len));
    assert_eq!(preview_report["applied"], 1);
    assert_eq!(preview_report["dry_run"], true);
    assert_eq!(list_len(preview, &namespace, &collection), 0);

    // 目标已存在：拒绝覆盖，且不发布摘要。
    let mut ignored_json = ptr::null_mut();
    let mut ignored_len = 0;
    // SAFETY: 句柄、路径与输出指针有效。
    let status = unsafe {
        shio_export(
            source,
            export_c.as_ptr(),
            ptr::null(),
            0,
            &mut ignored_json,
            &mut ignored_len,
        )
    };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(ignored_json.is_null());

    // 截断文件在写入前失败，目标库保持空。
    let text = std::fs::read_to_string(&export_path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.pop();
    std::fs::write(&truncated_path, lines.join("\n") + "\n").unwrap();
    let truncated_c = CString::new(truncated_path.to_str().unwrap()).unwrap();
    let third = open(&third_path, password, create);
    let mut json_pointer = ptr::null_mut();
    let mut json_len = 0;
    // SAFETY: 句柄、路径与输出指针有效。
    let status = unsafe {
        shio_import(
            third,
            truncated_c.as_ptr(),
            ptr::null(),
            0,
            &mut json_pointer,
            &mut json_len,
        )
    };
    assert_eq!(status, SHIO_CORRUPTED);
    assert!(json_pointer.is_null());
    assert_eq!(list_len(third, &namespace, &collection), 0);

    // 未知 mode 被拒绝。
    let bad_mode = br#"{"mode":"overwrite"}"#;
    // SAFETY: 句柄、路径、options 与输出指针有效。
    let status = unsafe {
        shio_import(
            third,
            export_c.as_ptr(),
            bad_mode.as_ptr(),
            bad_mode.len(),
            &mut json_pointer,
            &mut json_len,
        )
    };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);

    // SAFETY: 句柄有效。
    unsafe { shio_close(source) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(target) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(preview) };
    // SAFETY: 句柄有效。
    unsafe { shio_close(third) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(source) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(target) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(preview) };
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(third) };
}
