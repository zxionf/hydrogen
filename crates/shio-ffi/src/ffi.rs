//! C ABI 实现。
//!
//! 本模块是 workspace 内唯一允许 `unsafe` 的地方：只做裸指针校验、句柄转换、
//! 缓冲区所有权和 `catch_unwind`，不承载存储、加密或同步逻辑。

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::{json, Value};
use shio::{
    Credential, Kernel, ListOptions, OpenOptions, RecordId, RecordMeta, WriteOp, DEFAULT_LIST_LIMIT,
};
use tokio::runtime::{Builder, Runtime};
use zeroize::{Zeroize, Zeroizing};

use shio::Error;

/// ABI 版本；每次不兼容的 C ABI 变更都必须提升。
pub const SHIO_ABI_VERSION: u32 = 1;

/// 成功。
pub const SHIO_OK: i32 = 0;
/// 参数非法。
pub const SHIO_INVALID_ARGUMENT: i32 = -1;
/// 命名空间或集合名称非法。
pub const SHIO_INVALID_NAME: i32 = -2;
/// 口令错误或密钥材料损坏。
pub const SHIO_INVALID_KEY: i32 = -3;
/// 记录或数据库不存在。
pub const SHIO_NOT_FOUND: i32 = -4;
/// 句柄已关闭。
pub const SHIO_CLOSED: i32 = -5;
/// 只读句柄拒绝写操作。
pub const SHIO_READ_ONLY: i32 = -6;
/// 密钥轮换或升级未完成。
pub const SHIO_BUSY: i32 = -7;
/// 文件锁被其他进程占用。
pub const SHIO_LOCKED: i32 = -8;
/// 记录体超过上限。
pub const SHIO_VALUE_TOO_LARGE: i32 = -9;
/// 数据损坏或认证失败。
pub const SHIO_CORRUPTED: i32 = -10;
/// 未知格式版本。
pub const SHIO_UNSUPPORTED_FORMAT: i32 = -11;
/// JSON 序列化失败。
pub const SHIO_SERIALIZE: i32 = -12;
/// JSON 反序列化失败。
pub const SHIO_DESERIALIZE: i32 = -13;
/// 文件系统错误。
pub const SHIO_IO: i32 = -14;
/// 存储引擎错误。
pub const SHIO_DATABASE: i32 = -15;
/// 内部错误或 FFI 边界捕获到 panic。
pub const SHIO_INTERNAL: i32 = -16;
/// 同步未启用。
pub const SHIO_SYNC_DISABLED: i32 = -17;
/// 同步模式下禁止物理删除。
pub const SHIO_SYNC_ENABLED: i32 = -18;
/// 同步游标已过期，需要快照。
pub const SHIO_SYNC_RESYNC_REQUIRED: i32 = -19;
/// 缺少记录引用的密钥。
pub const SHIO_ROTATION_REQUIRED: i32 = -20;
/// 配对包已过期。
pub const SHIO_PAIRING_EXPIRED: i32 = -21;
/// 配对包无效。
pub const SHIO_PAIRING_INVALID: i32 = -22;

const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_BATCH_OPS: usize = 1000;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(message: impl Into<String>) {
    let cleaned = message.into().replace('\0', " ");
    if let Ok(message) = CString::new(cleaned) {
        LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(message));
    }
}

fn clear_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// 取回当前线程最近一次错误；返回包含 NUL 所需的字节数。
///
/// `buf` 为 `NULL` 或 `len == 0` 时只返回所需长度；否则最多写入 `len - 1`
/// 字节并补 NUL。`handle` 可以为 `NULL`，例如 `shio_open` 失败后。
///
/// # Safety
///
/// `buf` 非空时必须指向至少 `len` 字节可写内存。
#[no_mangle]
pub unsafe extern "C" fn shio_last_error(
    _handle: *const ShioKernel,
    buf: *mut c_char,
    len: usize,
) -> usize {
    let message = LAST_ERROR.with(|slot| slot.borrow().as_ref().map(|m| m.as_bytes().to_vec()));
    let Some(bytes) = message else {
        return 0;
    };
    let required = bytes.len().saturating_add(1);
    if buf.is_null() || len == 0 {
        return required;
    }

    let copied = bytes.len().min(len - 1);
    // SAFETY: 调用方保证 `buf` 至少有 `len` 字节可写；`copied + 1 <= len`。
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), buf, copied);
        *buf.add(copied) = 0;
    }
    required
}

/// ABI 版本查询；不需要打开数据库。
#[no_mangle]
pub extern "C" fn shio_abi_version() -> u32 {
    SHIO_ABI_VERSION
}

/// 不透明内核句柄。
pub struct ShioKernel {
    runtime: Runtime,
    kernel: Mutex<Option<Kernel>>,
}

impl ShioKernel {
    fn kernel(&self) -> shio::Result<Kernel> {
        self.kernel
            .lock()
            .map_err(|_| Error::Internal("ffi handle lock poisoned"))?
            .clone()
            .ok_or(Error::Closed)
    }

    fn take_kernel(&self) -> shio::Result<Option<Kernel>> {
        Ok(self
            .kernel
            .lock()
            .map_err(|_| Error::Internal("ffi handle lock poisoned"))?
            .take())
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

fn new_runtime() -> shio::Result<Runtime> {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)
}

fn ffi_guard<F>(operation: F) -> i32
where
    F: FnOnce() -> shio::Result<()>,
{
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
    match outcome {
        Ok(Ok(())) => {
            clear_error();
            SHIO_OK
        }
        Ok(Err(error)) => {
            set_error(error.to_string());
            status_for(&error)
        }
        Err(_) => {
            set_error("panic caught at the shio FFI boundary");
            SHIO_INTERNAL
        }
    }
}

fn status_for(error: &Error) -> i32 {
    match error {
        Error::InvalidArgument(_) => SHIO_INVALID_ARGUMENT,
        Error::InvalidName { .. } => SHIO_INVALID_NAME,
        Error::InvalidKey => SHIO_INVALID_KEY,
        Error::NotFound => SHIO_NOT_FOUND,
        Error::Closed => SHIO_CLOSED,
        Error::ReadOnly => SHIO_READ_ONLY,
        Error::Busy => SHIO_BUSY,
        Error::Locked => SHIO_LOCKED,
        Error::ValueTooLarge { .. } => SHIO_VALUE_TOO_LARGE,
        Error::Corrupted(_) => SHIO_CORRUPTED,
        Error::UnsupportedFormat { .. } => SHIO_UNSUPPORTED_FORMAT,
        Error::Serialize(_) => SHIO_SERIALIZE,
        Error::Deserialize(_) => SHIO_DESERIALIZE,
        Error::Io(_) => SHIO_IO,
        Error::Database(_) => SHIO_DATABASE,
        Error::SyncDisabled => SHIO_SYNC_DISABLED,
        Error::SyncEnabled => SHIO_SYNC_ENABLED,
        Error::SyncResyncRequired { .. } => SHIO_SYNC_RESYNC_REQUIRED,
        Error::RotationRequired { .. } => SHIO_ROTATION_REQUIRED,
        Error::PairingExpired => SHIO_PAIRING_EXPIRED,
        Error::PairingInvalid => SHIO_PAIRING_INVALID,
        Error::Internal(_) => SHIO_INTERNAL,
        _ => SHIO_INTERNAL,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OpenOptionsJson {
    create: bool,
    read_only: bool,
    max_value_size: Option<usize>,
    cache_size: Option<usize>,
}

fn parse_open_options(path: &str, json: &[u8]) -> shio::Result<OpenOptions> {
    let parsed = if json.is_empty() {
        OpenOptionsJson::default()
    } else {
        serde_json::from_slice(json).map_err(Error::Deserialize)?
    };

    let mut options = OpenOptions::new(PathBuf::from(path))
        .create(parsed.create)
        .read_only(parsed.read_only);
    if let Some(max) = parsed.max_value_size {
        options = options.max_value_size(max);
    }
    if let Some(cache) = parsed.cache_size {
        options = options.cache_size(cache);
    }
    options.validate()?;
    Ok(options)
}

/// # Safety
///
/// `pointer` 非空时必须指向至少 `len` 个可读字节，且在调用期间保持有效。
unsafe fn input_bytes<'a>(pointer: *const u8, len: usize) -> shio::Result<&'a [u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(Error::InvalidArgument("byte pointer is null"));
    }
    if len > MAX_INPUT_BYTES {
        return Err(Error::ValueTooLarge {
            size: len,
            max: MAX_INPUT_BYTES,
        });
    }
    // SAFETY: 由调用方契约保证指针和长度有效。
    Ok(unsafe { std::slice::from_raw_parts(pointer, len) })
}

/// # Safety
///
/// `pointer` 非空时必须是 NUL 结尾的 UTF-8 字符串。
unsafe fn input_str<'a>(pointer: *const c_char) -> shio::Result<&'a str> {
    if pointer.is_null() {
        return Err(Error::InvalidArgument("string pointer is null"));
    }
    // SAFETY: 由调用方契约保证指针是 NUL 结尾的可读字符串。
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map_err(|_| Error::InvalidArgument("string is not valid UTF-8"))
}

fn input_id(pointer: *const u8) -> shio::Result<RecordId> {
    if pointer.is_null() {
        return Err(Error::InvalidArgument("record id pointer is null"));
    }
    let mut bytes = [0u8; 16];
    // SAFETY: 由调用方契约保证 16 个字节可读。
    unsafe { ptr::copy_nonoverlapping(pointer, bytes.as_mut_ptr(), bytes.len()) };
    Ok(RecordId::from_bytes(bytes))
}

fn input_password(pointer: *const u8, len: usize) -> shio::Result<String> {
    // SAFETY: 由调用方契约保证指针和长度有效。
    let bytes = unsafe { input_bytes(pointer, len)? };
    let owned = Zeroizing::new(bytes.to_vec());
    std::str::from_utf8(owned.as_slice())
        .map(str::to_owned)
        .map_err(|_| Error::InvalidArgument("password is not valid UTF-8"))
}

fn reset_output(out: *mut *mut u8, out_len: *mut usize) -> shio::Result<()> {
    if out.is_null() || out_len.is_null() {
        return Err(Error::InvalidArgument("output pointer is null"));
    }
    // SAFETY: 调用方提供了两个非空输出指针。
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    Ok(())
}

fn set_output(bytes: Vec<u8>, out: *mut *mut u8, out_len: *mut usize) -> shio::Result<()> {
    reset_output(out, out_len)?;
    if bytes.is_empty() {
        return Ok(());
    }

    let mut boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let pointer = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    // SAFETY: 两个输出指针已由 `reset_output` 校验。
    unsafe {
        *out = pointer;
        *out_len = len;
    }
    Ok(())
}

/// 释放由 shio 返回的缓冲区，并在释放前清零；`ptr` 与 `len` 必须成对。
///
/// # Safety
///
/// `pointer`/`len` 必须来自一次成功的 shio 输出调用，且只能释放一次。
#[no_mangle]
pub unsafe extern "C" fn shio_free(pointer: *mut u8, len: usize) {
    if pointer.is_null() {
        return;
    }
    // SAFETY: 由调用方契约保证 `pointer`/`len` 来自一次 `set_output`。
    unsafe {
        let slice = ptr::slice_from_raw_parts_mut(pointer, len);
        let mut boxed = Box::from_raw(slice);
        boxed.as_mut().zeroize();
        drop(boxed);
    }
}

fn meta_value(meta: &RecordMeta) -> Value {
    json!({
        "id": meta.id.to_string(),
        "rev": meta.rev,
        "created_at": meta.created_at.as_millis(),
        "updated_at": meta.updated_at.as_millis(),
        "data_version": meta.data_version,
        "deleted": meta.deleted,
        "ciphertext_len": meta.ciphertext_len,
    })
}

fn value_to_bytes(value: &Value) -> shio::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(Error::Serialize)
}

/// 打开或创建数据库。
///
/// `options_json` 可为空；字段为 `create`、`read_only`、`max_value_size`、
/// `cache_size`。成功时写入不透明句柄，失败时写入 `NULL`。
///
/// # Safety
///
/// 字符串必须是 NUL 结尾的 UTF-8；字节指针必须指向对应长度；`out_handle`
/// 必须可写。
#[no_mangle]
pub unsafe extern "C" fn shio_open(
    path: *const c_char,
    password: *const u8,
    password_len: usize,
    options_json: *const u8,
    options_len: usize,
    out_handle: *mut *mut ShioKernel,
) -> i32 {
    ffi_guard(|| {
        if out_handle.is_null() {
            return Err(Error::InvalidArgument("output handle is null"));
        }
        // SAFETY: 调用方提供了非空输出指针。
        unsafe {
            *out_handle = ptr::null_mut();
        }

        // SAFETY: 由调用方契约保证输入有效。
        let path = unsafe { input_str(path)? };
        let password = input_password(password, password_len)?;
        // SAFETY: 由调用方契约保证 options 指针有效。
        let options_bytes = unsafe { input_bytes(options_json, options_len)? };
        let options = parse_open_options(path, options_bytes)?;

        let runtime = new_runtime()?;
        let kernel = runtime.block_on(Kernel::open(options, Credential::password(password)))?;
        let boxed = Box::new(ShioKernel {
            runtime,
            kernel: Mutex::new(Some(kernel)),
        });
        // SAFETY: 调用方提供了非空输出指针。
        unsafe {
            *out_handle = Box::into_raw(boxed);
        }
        Ok(())
    })
}

/// 关闭句柄并等待在途操作；不释放句柄本身。可重复调用。
///
/// # Safety
///
/// `handle` 必须来自 `shio_open`，且在 destroy 前保持有效。
#[no_mangle]
pub unsafe extern "C" fn shio_close(handle: *mut ShioKernel) -> i32 {
    ffi_guard(|| {
        // SAFETY: 调用方保证句柄指针来自 `shio_open` 且未被 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        if let Some(kernel) = handle.take_kernel()? {
            handle.block_on(kernel.close())?;
        }
        Ok(())
    })
}

/// 释放句柄；会先关闭仍在打开的句柄。调用后指针失效。
///
/// # Safety
///
/// `handle` 必须来自 `shio_open`，且调用时没有其它线程继续使用它。
#[no_mangle]
pub unsafe extern "C" fn shio_kernel_destroy(handle: *mut ShioKernel) {
    if handle.is_null() {
        return;
    }
    let _ = ffi_guard(|| {
        // SAFETY: 调用方保证句柄指针来自 `shio_open` 且只 destroy 一次。
        let handle = unsafe { Box::from_raw(handle) };
        if let Some(kernel) = handle.take_kernel()? {
            handle.block_on(kernel.close())?;
        }
        Ok(())
    });
}

/// 插入一条记录，返回 16 字节 UUIDv7 和元信息 JSON。
///
/// # Safety
///
/// 句柄、名称、JSON 与输出指针必须按 `include/shio.h` 的契约有效。
#[no_mangle]
pub unsafe extern "C" fn shio_insert(
    handle: *mut ShioKernel,
    namespace: *const c_char,
    collection: *const c_char,
    json: *const u8,
    json_len: usize,
    out_id: *mut u8,
    out_meta: *mut *mut u8,
    out_meta_len: *mut usize,
) -> i32 {
    ffi_guard(|| {
        reset_output(out_meta, out_meta_len)?;
        if out_id.is_null() {
            return Err(Error::InvalidArgument("record id output is null"));
        }
        // SAFETY: 由调用方契约保证输入有效。
        let namespace = unsafe { input_str(namespace)? };
        // SAFETY: 由调用方契约保证输入有效。
        let collection = unsafe { input_str(collection)? };
        // SAFETY: 由调用方契约保证输入有效。
        let payload = unsafe { input_bytes(json, json_len)? };
        let value: Value = serde_json::from_slice(payload).map_err(Error::Deserialize)?;

        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let collection = kernel
            .namespace(namespace)?
            .collection::<Value>(collection)?;
        let (id, meta) = handle.block_on(collection.insert(&value))?;

        let meta = value_to_bytes(&meta_value(&meta))?;
        set_output(meta, out_meta, out_meta_len)?;
        // SAFETY: 调用方保证 `out_id` 指向至少 16 字节可写。
        unsafe {
            ptr::copy_nonoverlapping(id.as_bytes().as_ptr(), out_id, 16);
        }
        Ok(())
    })
}

/// 指定 ID 的 upsert，返回元信息 JSON。
///
/// # Safety
///
/// 句柄、名称、ID、JSON 与输出指针必须按 `include/shio.h` 的契约有效。
#[no_mangle]
pub unsafe extern "C" fn shio_put(
    handle: *mut ShioKernel,
    namespace: *const c_char,
    collection: *const c_char,
    id: *const u8,
    json: *const u8,
    json_len: usize,
    out_meta: *mut *mut u8,
    out_meta_len: *mut usize,
) -> i32 {
    ffi_guard(|| {
        reset_output(out_meta, out_meta_len)?;
        // SAFETY: 由调用方契约保证输入有效。
        let namespace = unsafe { input_str(namespace)? };
        // SAFETY: 由调用方契约保证输入有效。
        let collection = unsafe { input_str(collection)? };
        let id = input_id(id)?;
        // SAFETY: 由调用方契约保证输入有效。
        let payload = unsafe { input_bytes(json, json_len)? };
        let value: Value = serde_json::from_slice(payload).map_err(Error::Deserialize)?;

        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let collection = kernel
            .namespace(namespace)?
            .collection::<Value>(collection)?;
        let meta = handle.block_on(collection.put(id, &value))?;
        set_output(value_to_bytes(&meta_value(&meta))?, out_meta, out_meta_len)
    })
}

/// 读取一条记录，返回 `{"meta":{...},"value":...}`；墓碑视为不存在。
///
/// # Safety
///
/// 句柄、名称、ID 与输出指针必须按 `include/shio.h` 的契约有效。
#[no_mangle]
pub unsafe extern "C" fn shio_get(
    handle: *mut ShioKernel,
    namespace: *const c_char,
    collection: *const c_char,
    id: *const u8,
    out_json: *mut *mut u8,
    out_json_len: *mut usize,
) -> i32 {
    ffi_guard(|| {
        reset_output(out_json, out_json_len)?;
        // SAFETY: 由调用方契约保证输入有效。
        let namespace = unsafe { input_str(namespace)? };
        // SAFETY: 由调用方契约保证输入有效。
        let collection = unsafe { input_str(collection)? };
        let id = input_id(id)?;

        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let collection = kernel
            .namespace(namespace)?
            .collection::<Value>(collection)?;
        let stored = handle
            .block_on(collection.get(&id))?
            .ok_or(Error::NotFound)?;
        let value = json!({
            "meta": meta_value(&stored.meta),
            "value": stored.value,
        });
        set_output(value_to_bytes(&value)?, out_json, out_json_len)
    })
}

/// 写墓碑；`out_existed` 可为 `NULL`。
///
/// # Safety
///
/// 句柄、名称和 ID 必须按 `include/shio.h` 的契约有效；`out_existed` 非空时必须可写。
#[no_mangle]
pub unsafe extern "C" fn shio_delete(
    handle: *mut ShioKernel,
    namespace: *const c_char,
    collection: *const c_char,
    id: *const u8,
    out_existed: *mut i32,
) -> i32 {
    ffi_guard(|| {
        if !out_existed.is_null() {
            // SAFETY: 调用方提供了非空输出指针。
            unsafe {
                *out_existed = 0;
            }
        }
        // SAFETY: 由调用方契约保证输入有效。
        let namespace = unsafe { input_str(namespace)? };
        // SAFETY: 由调用方契约保证输入有效。
        let collection = unsafe { input_str(collection)? };
        let id = input_id(id)?;

        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let collection = kernel
            .namespace(namespace)?
            .collection::<Value>(collection)?;
        let existed = handle.block_on(collection.delete(&id))?;
        if !out_existed.is_null() {
            // SAFETY: 调用方提供了非空输出指针。
            unsafe {
                *out_existed = i32::from(existed);
            }
        }
        Ok(())
    })
}

/// 按记录 ID 升序分页；返回 `{"items":[...],"next":...}`。
///
/// `after` 为 `NULL` 表示从头；`limit == 0` 使用默认值。
///
/// # Safety
///
/// 句柄、名称和输出指针必须按 `include/shio.h` 的契约有效；`after` 非空时指向 16 字节 ID。
#[no_mangle]
pub unsafe extern "C" fn shio_list(
    handle: *mut ShioKernel,
    namespace: *const c_char,
    collection: *const c_char,
    after: *const u8,
    limit: usize,
    include_deleted: u8,
    out_json: *mut *mut u8,
    out_json_len: *mut usize,
) -> i32 {
    ffi_guard(|| {
        reset_output(out_json, out_json_len)?;
        // SAFETY: 由调用方契约保证输入有效。
        let namespace = unsafe { input_str(namespace)? };
        // SAFETY: 由调用方契约保证输入有效。
        let collection = unsafe { input_str(collection)? };
        let after = if after.is_null() {
            None
        } else {
            Some(input_id(after)?)
        };
        let limit = if limit == 0 {
            DEFAULT_LIST_LIMIT
        } else {
            limit
        };
        let options = ListOptions {
            after,
            limit,
            include_deleted: include_deleted != 0,
        };
        options.validate()?;

        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let collection = kernel
            .namespace(namespace)?
            .collection::<Value>(collection)?;
        let page = handle.block_on(collection.list(options))?;
        let items: Vec<Value> = page.items.iter().map(meta_value).collect();
        let next = page.next.map(|id| id.to_string());
        let value = json!({ "items": items, "next": next });
        set_output(value_to_bytes(&value)?, out_json, out_json_len)
    })
}

/// 原子批量写；输入为 `[{"id":"uuid","value":...},{"id":"uuid","delete":true}]`。
///
/// # Safety
///
/// 句柄、名称、JSON 与输出指针必须按 `include/shio.h` 的契约有效。
#[no_mangle]
pub unsafe extern "C" fn shio_batch(
    handle: *mut ShioKernel,
    namespace: *const c_char,
    collection: *const c_char,
    ops_json: *const u8,
    ops_json_len: usize,
    out_meta: *mut *mut u8,
    out_meta_len: *mut usize,
) -> i32 {
    ffi_guard(|| {
        reset_output(out_meta, out_meta_len)?;
        // SAFETY: 由调用方契约保证输入有效。
        let namespace = unsafe { input_str(namespace)? };
        // SAFETY: 由调用方契约保证输入有效。
        let collection = unsafe { input_str(collection)? };
        // SAFETY: 由调用方契约保证输入有效。
        let payload = unsafe { input_bytes(ops_json, ops_json_len)? };
        let value: Value = serde_json::from_slice(payload).map_err(Error::Deserialize)?;
        let items = value
            .as_array()
            .ok_or(Error::InvalidArgument("batch input must be a JSON array"))?;
        if items.len() > MAX_BATCH_OPS {
            return Err(Error::InvalidArgument("batch has too many operations"));
        }

        let mut ops = Vec::with_capacity(items.len());
        for item in items {
            let object = item
                .as_object()
                .ok_or(Error::InvalidArgument("batch item must be an object"))?;
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidArgument("batch item id must be a string"))?
                .parse::<RecordId>()?;
            let delete = match object.get("delete") {
                Some(value) => value
                    .as_bool()
                    .ok_or(Error::InvalidArgument("batch delete must be a boolean"))?,
                None => false,
            };
            if delete {
                if object.contains_key("value") {
                    return Err(Error::InvalidArgument(
                        "batch delete item must not contain value",
                    ));
                }
                ops.push(WriteOp::Delete { id });
            } else {
                let value = object
                    .get("value")
                    .ok_or(Error::InvalidArgument("batch put item requires value"))?
                    .clone();
                ops.push(WriteOp::Put { id, value });
            }
        }

        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let collection = kernel
            .namespace(namespace)?
            .collection::<Value>(collection)?;
        let metas = handle.block_on(collection.batch(ops))?;
        let values: Vec<Value> = metas.iter().map(meta_value).collect();
        set_output(
            value_to_bytes(&Value::Array(values))?,
            out_meta,
            out_meta_len,
        )
    })
}

/// 改口令；不重写记录。
///
/// # Safety
///
/// 句柄有效，口令指针指向 `new_password_len` 字节且为 UTF-8。
#[no_mangle]
pub unsafe extern "C" fn shio_change_password(
    handle: *mut ShioKernel,
    new_password: *const u8,
    new_password_len: usize,
) -> i32 {
    ffi_guard(|| {
        let password = input_password(new_password, new_password_len)?;
        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        let credential = Credential::password(password);
        let Credential::Password(secret) = credential;
        handle.block_on(kernel.change_password(secret))
    })
}

/// 轮换数据密钥。
///
/// # Safety
///
/// `handle` 必须来自 `shio_open`，且在 destroy 前保持有效。
#[no_mangle]
pub unsafe extern "C" fn shio_rotate_data_key(handle: *mut ShioKernel) -> i32 {
    ffi_guard(|| {
        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        handle.block_on(kernel.rotate_data_key())
    })
}

/// 持久化屏障。
///
/// # Safety
///
/// `handle` 必须来自 `shio_open`，且在 destroy 前保持有效。
#[no_mangle]
pub unsafe extern "C" fn shio_flush(handle: *mut ShioKernel) -> i32 {
    ffi_guard(|| {
        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let kernel = handle.kernel()?;
        handle.block_on(kernel.flush())
    })
}

/// 统计信息 JSON。
///
/// # Safety
///
/// 句柄与输出指针必须按 `include/shio.h` 的契约有效。
#[no_mangle]
pub unsafe extern "C" fn shio_stats(
    handle: *mut ShioKernel,
    out_json: *mut *mut u8,
    out_json_len: *mut usize,
) -> i32 {
    ffi_guard(|| {
        reset_output(out_json, out_json_len)?;
        // SAFETY: 调用方保证句柄来自 `shio_open` 且未 destroy。
        let handle =
            unsafe { handle.as_ref() }.ok_or(Error::InvalidArgument("kernel handle is null"))?;
        let stats = handle.kernel()?.stats()?;
        let value = json!({
            "format_version": stats.format_version,
            "key_id": stats.key_id.to_string(),
            "db_bytes": stats.db_bytes,
        });
        set_output(value_to_bytes(&value)?, out_json, out_json_len)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_is_positive() {
        assert!(shio_abi_version() > 0);
    }
}
