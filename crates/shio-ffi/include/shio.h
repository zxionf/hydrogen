/*
 * shio C ABI v1
 *
 * 所有函数都是阻塞调用：open/close、轮换、改口令和写操作可能等待磁盘与 KDF。
 * 不要在 Android UI 线程直接调用；Kotlin/Java 侧使用 worker 线程。
 *
 * 数据所有权：
 * - 输入字符串以 NUL 结尾的 UTF-8；字节输入显式带长度，允许内嵌 NUL。
 * - 输出的 uint8_t* 由 shio 分配，必须用 shio_free(ptr, len) 释放。
 * - 失败时输出指针为 NULL、长度为 0；错误详情用 shio_last_error 取。
 *
 * 句柄生命周期：
 * - shio_open 成功后返回不透明句柄。
 * - shio_close 关闭并等待在途操作，但不释放句柄；可重复调用。
 * - shio_kernel_destroy 释放句柄；调用前必须确保没有其它线程继续使用它。
 */
#ifndef SHIO_H
#define SHIO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define SHIO_ABI_VERSION 1u

/* 成功。 */
#define SHIO_OK 0
/* 参数非法。 */
#define SHIO_INVALID_ARGUMENT (-1)
/* 命名空间或集合名称非法。 */
#define SHIO_INVALID_NAME (-2)
/* 口令错误或密钥材料损坏。 */
#define SHIO_INVALID_KEY (-3)
/* 记录或数据库不存在。 */
#define SHIO_NOT_FOUND (-4)
/* 句柄已关闭。 */
#define SHIO_CLOSED (-5)
/* 只读句柄拒绝写操作。 */
#define SHIO_READ_ONLY (-6)
/* 密钥轮换或升级未完成。 */
#define SHIO_BUSY (-7)
/* 文件锁被其他进程占用。 */
#define SHIO_LOCKED (-8)
/* 记录体超过上限。 */
#define SHIO_VALUE_TOO_LARGE (-9)
/* 数据损坏或认证失败。 */
#define SHIO_CORRUPTED (-10)
/* 未知格式版本。 */
#define SHIO_UNSUPPORTED_FORMAT (-11)
/* JSON 序列化失败。 */
#define SHIO_SERIALIZE (-12)
/* JSON 反序列化失败。 */
#define SHIO_DESERIALIZE (-13)
/* 文件系统错误。 */
#define SHIO_IO (-14)
/* 存储引擎错误。 */
#define SHIO_DATABASE (-15)
/* 内部错误或 FFI 边界捕获到 panic。 */
#define SHIO_INTERNAL (-16)
/* 同步未启用。 */
#define SHIO_SYNC_DISABLED (-17)
/* 同步模式下禁止物理删除。 */
#define SHIO_SYNC_ENABLED (-18)
/* 同步游标已过期，需要快照。 */
#define SHIO_SYNC_RESYNC_REQUIRED (-19)
/* 缺少记录引用的密钥。 */
#define SHIO_ROTATION_REQUIRED (-20)
/* 配对包已过期。 */
#define SHIO_PAIRING_EXPIRED (-21)
/* 配对包无效。 */
#define SHIO_PAIRING_INVALID (-22)

typedef struct shio_kernel shio_kernel;

/* 返回 ABI 版本；不需要打开数据库。 */
uint32_t shio_abi_version(void);

/*
 * 取当前线程最近一次错误。返回包含 NUL 的所需字节数。
 * handle 可为 NULL；buf 为 NULL 或 len 为 0 时只查询长度。
 */
size_t shio_last_error(const shio_kernel *handle, char *buf, size_t len);

/*
 * 打开或创建数据库。
 * password 必须是 UTF-8 字节串；options_json 可为 NULL/0，支持字段：
 *   create(bool)、read_only(bool)、max_value_size(number)、cache_size(number)。
 */
int32_t shio_open(const char *path,
                  const uint8_t *password,
                  size_t password_len,
                  const uint8_t *options_json,
                  size_t options_len,
                  shio_kernel **out_handle);

/* 关闭并等待在途操作；不释放句柄，可重复调用。 */
int32_t shio_close(shio_kernel *handle);

/* 释放句柄；会先关闭。调用后指针失效。 */
void shio_kernel_destroy(shio_kernel *handle);

/* 释放 shio 返回的缓冲区，并在释放前清零。 */
void shio_free(void *ptr, size_t len);

/* 插入一条 JSON 记录；out_id 必须指向 16 字节。 */
int32_t shio_insert(shio_kernel *handle,
                    const char *namespace_name,
                    const char *collection_name,
                    const uint8_t *json,
                    size_t json_len,
                    uint8_t *out_id,
                    uint8_t **out_meta,
                    size_t *out_meta_len);

/* 指定 16 字节记录 ID 的 upsert。 */
int32_t shio_put(shio_kernel *handle,
                 const char *namespace_name,
                 const char *collection_name,
                 const uint8_t *id,
                 const uint8_t *json,
                 size_t json_len,
                 uint8_t **out_meta,
                 size_t *out_meta_len);

/* 读取记录，返回 {"meta":{...},"value":...}；墓碑视为不存在。 */
int32_t shio_get(shio_kernel *handle,
                 const char *namespace_name,
                 const char *collection_name,
                 const uint8_t *id,
                 uint8_t **out_json,
                 size_t *out_json_len);

/* 写墓碑；out_existed 可为 NULL。 */
int32_t shio_delete(shio_kernel *handle,
                    const char *namespace_name,
                    const char *collection_name,
                    const uint8_t *id,
                    int32_t *out_existed);

/*
 * 按记录 ID 升序分页，返回 {"items":[...],"next":...}。
 * after 可为 NULL；limit 为 0 时使用默认值。
 */
int32_t shio_list(shio_kernel *handle,
                  const char *namespace_name,
                  const char *collection_name,
                  const uint8_t *after,
                  size_t limit,
                  uint8_t include_deleted,
                  uint8_t **out_json,
                  size_t *out_json_len);

/*
 * 原子批量写。输入 JSON 数组：
 *   [{"id":"uuid","value":...},{"id":"uuid","delete":true}]
 * 返回元信息 JSON 数组。
 */
int32_t shio_batch(shio_kernel *handle,
                   const char *namespace_name,
                   const char *collection_name,
                   const uint8_t *ops_json,
                   size_t ops_json_len,
                   uint8_t **out_meta,
                   size_t *out_meta_len);

/* 改口令；不重写记录。new_password 必须是 UTF-8。 */
int32_t shio_change_password(shio_kernel *handle,
                             const uint8_t *new_password,
                             size_t new_password_len);

/* 轮换数据密钥。 */
int32_t shio_rotate_data_key(shio_kernel *handle);

/* 持久化屏障。 */
int32_t shio_flush(shio_kernel *handle);

/* 统计信息 JSON：format_version、key_id、db_bytes。 */
int32_t shio_stats(shio_kernel *handle,
                   uint8_t **out_json,
                   size_t *out_json_len);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SHIO_H */
