//! C ABI 同步冒烟测试：配对、变更帧、快照帧与轮换只走公开符号。
//!
//! Kotlin 不解析 SHY2 帧；本测试也只把帧当不透明字节搬运。

#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;

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

    let mut migrated = 0;
    // SAFETY: 句柄、key ID 与输出指针有效。
    let status = unsafe { shio_sync_migrate_all(a, hex_bytes(&to).as_ptr(), &mut migrated) };
    assert_eq!(status, SHIO_OK, "migrate_all failed: {}", last_error());
    assert!(migrated >= 1);

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
