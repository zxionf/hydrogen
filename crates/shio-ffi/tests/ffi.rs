//! C ABI 冒烟测试：只通过公开符号走 open/CRUD/batch/轮换/改口令/关闭。

#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Barrier};
use std::thread;

use serde_json::Value;
use shio_ffi::*;

fn last_error() -> String {
    let mut buffer = vec![0 as c_char; 1024];
    // SAFETY: 缓冲区长度与指针匹配；句柄允许为 NULL。
    let required = unsafe { shio_last_error(ptr::null(), buffer.as_mut_ptr(), buffer.len()) };
    if required == 0 {
        return String::new();
    }
    // SAFETY: shio_last_error 在缓冲区足够时补 NUL；这里只读取该 NUL 之前的内容。
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

fn parse(bytes: Vec<u8>) -> Value {
    serde_json::from_slice(&bytes).expect("shio returned invalid JSON")
}

#[test]
fn crud_batch_rotation_and_password_change() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.db");
    let create = br#"{"create":true}"#;
    let handle = open(&path, b"first-password", create);

    assert_eq!(shio_abi_version(), SHIO_ABI_VERSION);
    // SAFETY: shio_version 返回进程级静态的 NUL 结尾字符串。
    let kernel_version = unsafe { CStr::from_ptr(shio_version()) };
    assert_eq!(kernel_version.to_str().unwrap(), shio::version());

    let namespace = CString::new("notes").unwrap();
    let collection = CString::new("items").unwrap();

    let mut stats_ptr = ptr::null_mut();
    let mut stats_len = 0;
    // SAFETY: 句柄与输出指针有效。
    let status = unsafe { shio_stats(handle, &mut stats_ptr, &mut stats_len) };
    assert_eq!(status, SHIO_OK, "stats failed: {}", last_error());
    let stats = parse(take_buffer(stats_ptr, stats_len));
    assert!(stats["format_version"].as_u64().unwrap() >= 1);
    assert_eq!(stats["key_id"].as_str().unwrap().len(), 32);

    let payload = br#"{"title":"alpha","n":1}"#;
    let mut id = [0u8; 16];
    let mut meta_ptr = ptr::null_mut();
    let mut meta_len = 0;
    // SAFETY: 句柄、名称、载荷和输出指针有效。
    let status = unsafe {
        shio_insert(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            id.as_mut_ptr(),
            &mut meta_ptr,
            &mut meta_len,
        )
    };
    assert_eq!(status, SHIO_OK, "insert failed: {}", last_error());
    let inserted = parse(take_buffer(meta_ptr, meta_len));
    assert_eq!(inserted["id"].as_str().unwrap().len(), 36);
    assert_eq!(inserted["deleted"], false);

    let mut get_ptr = ptr::null_mut();
    let mut get_len = 0;
    // SAFETY: 句柄、名称、ID 和输出指针有效。
    let status = unsafe {
        shio_get(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            id.as_ptr(),
            &mut get_ptr,
            &mut get_len,
        )
    };
    assert_eq!(status, SHIO_OK, "get failed: {}", last_error());
    let got = parse(take_buffer(get_ptr, get_len));
    assert_eq!(got["value"]["title"], "alpha");
    assert_eq!(got["meta"]["id"], inserted["id"]);

    let explicit_id = [9u8; 16];
    let mut put_meta_ptr = ptr::null_mut();
    let mut put_meta_len = 0;
    // SAFETY: 句柄、名称、ID、载荷和输出指针有效。
    let status = unsafe {
        shio_put(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            explicit_id.as_ptr(),
            br#"{"title":"beta"}"#.as_ptr(),
            br#"{"title":"beta"}"#.len(),
            &mut put_meta_ptr,
            &mut put_meta_len,
        )
    };
    assert_eq!(status, SHIO_OK, "put failed: {}", last_error());
    let put_meta = parse(take_buffer(put_meta_ptr, put_meta_len));
    assert_eq!(put_meta["id"], "09090909-0909-0909-0909-090909090909");

    let batch = format!(
        r#"[{{"id":"{}","value":{{"title":"alpha2"}}}},{{"id":"09090909-0909-0909-0909-090909090909","delete":true}}]"#,
        inserted["id"].as_str().unwrap()
    );
    let mut batch_ptr = ptr::null_mut();
    let mut batch_len = 0;
    // SAFETY: 句柄、名称、批处理和输出指针有效。
    let status = unsafe {
        shio_batch(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            batch.as_ptr(),
            batch.len(),
            &mut batch_ptr,
            &mut batch_len,
        )
    };
    assert_eq!(status, SHIO_OK, "batch failed: {}", last_error());
    let metas = parse(take_buffer(batch_ptr, batch_len));
    assert_eq!(metas.as_array().unwrap().len(), 2);

    let mut list_ptr = ptr::null_mut();
    let mut list_len = 0;
    // SAFETY: 句柄、名称和输出指针有效。
    let status = unsafe {
        shio_list(
            handle,
            namespace.as_ptr(),
            collection.as_ptr(),
            ptr::null(),
            10,
            0,
            &mut list_ptr,
            &mut list_len,
        )
    };
    assert_eq!(status, SHIO_OK, "list failed: {}", last_error());
    let page = parse(take_buffer(list_ptr, list_len));
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["id"], inserted["id"]);
    assert_eq!(page["next"], Value::Null);

    // SAFETY: 句柄有效。
    let status = unsafe { shio_flush(handle) };
    assert_eq!(status, SHIO_OK, "flush failed: {}", last_error());
    // SAFETY: 句柄有效。
    let status = unsafe { shio_rotate_data_key(handle) };
    assert_eq!(status, SHIO_OK, "rotation failed: {}", last_error());

    let new_password = b"second-password";
    // SAFETY: 句柄和口令缓冲区有效。
    let status = unsafe { shio_change_password(handle, new_password.as_ptr(), new_password.len()) };
    assert_eq!(status, SHIO_OK, "change password failed: {}", last_error());

    // SAFETY: 句柄有效；重复关闭必须成功。
    assert_eq!(unsafe { shio_close(handle) }, SHIO_OK);
    assert_eq!(unsafe { shio_close(handle) }, SHIO_OK);
    // SAFETY: 句柄在 destroy 前仍有效，但已经关闭。
    assert_eq!(
        unsafe { shio_flush(handle) },
        SHIO_CLOSED,
        "closed handle should reject operations"
    );
    // SAFETY: 关闭后 destroy，且没有其它线程继续使用。
    unsafe { shio_kernel_destroy(handle) };

    let no_create = br#"{}"#;
    let mut stale = ptr::null_mut();
    let path_c = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: 所有指针有效；预期旧口令失败。
    let status = unsafe {
        shio_open(
            path_c.as_ptr(),
            b"first-password".as_ptr(),
            b"first-password".len(),
            no_create.as_ptr(),
            no_create.len(),
            &mut stale,
        )
    };
    assert_eq!(status, SHIO_INVALID_KEY, "old password should fail");
    assert!(stale.is_null());
    assert!(!last_error().is_empty());

    let reopened = open(&path, new_password, no_create);
    // SAFETY: 句柄有效。
    unsafe { shio_close(reopened) };
    // SAFETY: 关闭后 destroy。
    unsafe { shio_kernel_destroy(reopened) };
}

#[test]
fn close_waits_for_inflight_calls_and_rejects_new_ones() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("race.db");
    let handle = open(&path, b"pw", br#"{"create":true}"#);
    let address = handle as usize;
    let namespace = Arc::new(CString::new("race").unwrap());
    let collection = Arc::new(CString::new("items").unwrap());
    let barrier = Arc::new(Barrier::new(2));

    let worker_barrier = Arc::clone(&barrier);
    let worker_namespace = Arc::clone(&namespace);
    let worker_collection = Arc::clone(&collection);
    let worker = thread::spawn(move || {
        let handle = address as *mut ShioKernel;
        worker_barrier.wait();
        let mut statuses = Vec::new();
        for index in 0..128u8 {
            let id = [index; 16];
            let payload = br#"{"race":true}"#;
            let mut meta_ptr = ptr::null_mut();
            let mut meta_len = 0;
            // SAFETY: 句柄地址在 worker 结束前保持有效；输入和输出指针有效。
            let status = unsafe {
                shio_put(
                    handle,
                    worker_namespace.as_ptr(),
                    worker_collection.as_ptr(),
                    id.as_ptr(),
                    payload.as_ptr(),
                    payload.len(),
                    &mut meta_ptr,
                    &mut meta_len,
                )
            };
            if status == SHIO_OK {
                let _ = take_buffer(meta_ptr, meta_len);
            } else {
                assert_eq!(status, SHIO_CLOSED, "unexpected status: {status}");
                assert!(!last_error().is_empty());
            }
            statuses.push(status);
        }
        statuses
    });

    barrier.wait();
    // SAFETY: 句柄有效；close 与 worker 的在途调用并发。
    assert_eq!(unsafe { shio_close(handle) }, SHIO_OK);
    let statuses = worker.join().unwrap();
    assert!(statuses
        .iter()
        .all(|status| *status == SHIO_OK || *status == SHIO_CLOSED));
    // SAFETY: worker 已结束，句柄只在这里 destroy 一次。
    unsafe { shio_kernel_destroy(handle) };
}

#[test]
fn rejects_invalid_arguments_and_reports_errors() {
    let mut handle = ptr::null_mut();
    // SAFETY: 句柄输出可写；路径为 NULL 应被拒绝。
    let status = unsafe {
        shio_open(
            ptr::null(),
            b"pw".as_ptr(),
            b"pw".len(),
            ptr::null(),
            0,
            &mut handle,
        )
    };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(handle.is_null());
    assert!(!last_error().is_empty());

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty.db");
    let invalid_options = br#"{"create":true,"unknown":1}"#;
    let mut handle = ptr::null_mut();
    let path_c = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: 所有指针有效；未知选项应被拒绝。
    let status = unsafe {
        shio_open(
            path_c.as_ptr(),
            b"pw".as_ptr(),
            b"pw".len(),
            invalid_options.as_ptr(),
            invalid_options.len(),
            &mut handle,
        )
    };
    assert_eq!(status, SHIO_DESERIALIZE);
    assert!(handle.is_null());

    // SAFETY: NULL 句柄应被拒绝。
    let status = unsafe { shio_flush(ptr::null_mut()) };
    assert_eq!(status, SHIO_INVALID_ARGUMENT);
    assert!(!last_error().is_empty());
}
