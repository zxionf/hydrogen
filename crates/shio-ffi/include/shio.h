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
/* 操作在批次边界被取消。 */
#define SHIO_CANCELLED (-23)

typedef struct shio_kernel shio_kernel;

/* 返回 ABI 版本；不需要打开数据库。 */
uint32_t shio_abi_version(void);

/*
 * 返回内核版本（编译期包版本），NUL 结尾 UTF-8。
 * 指向进程级静态存储，调用方不得释放；不需要句柄和数据库。
 */
const char *shio_version(void);

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

/*
 * 在线备份；目标必须不存在。
 * 返回摘要 JSON：{"format_version":N,"db_bytes":N,"table_count":N,"row_count":N}。
 */
int32_t shio_backup(shio_kernel *handle,
                    const char *out_path,
                    uint8_t **out_json,
                    size_t *out_json_len);

/*
 * 从备份恢复到 target_path；目标必须不存在或为空。
 * options_json 同 shio_open；restore 会忽略 create。成功返回已打开的目标句柄。
 */
int32_t shio_restore(const char *backup_path,
                     const char *target_path,
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

/* ---- P4 同步扩展；以下函数均为阻塞调用 ---- */

/*
 * 启用同步；device_id 为 NULL 时生成随机设备 ID。
 * 返回 sync_info JSON；首次真实建库应紧接着调用并检查结果。
 */
int32_t shio_sync_enable(shio_kernel *handle,
                         const uint8_t *device_id,
                         uint8_t **out_json,
                         size_t *out_json_len);

/* 同步摘要 JSON；未启用同步返回 SHIO_SYNC_DISABLED。 */
int32_t shio_sync_info(shio_kernel *handle,
                       uint8_t **out_json,
                       size_t *out_json_len);

/*
 * 读取 after 之后的变更页，返回 SHY2 帧。
 * after=0 表示从头；limit=0 使用默认 100，最大 1000。
 * out_next 为下一页游标，out_has_more 表示是否还有更多；两者可为 NULL。
 */
int32_t shio_sync_changes(shio_kernel *handle,
                          uint64_t after,
                          size_t limit,
                          uint8_t **out_frame,
                          size_t *out_frame_len,
                          uint64_t *out_next,
                          uint8_t *out_has_more);

/* 应用变更帧；返回 {"applied":N,"skipped":N,"controls":N}。 */
int32_t shio_sync_apply(shio_kernel *handle,
                        const uint8_t *frame,
                        size_t frame_len,
                        uint8_t **out_report_json,
                        size_t *out_report_len);

/* 本机待发送的轮换 offer 帧。 */
int32_t shio_sync_offers(shio_kernel *handle,
                         uint8_t **out_frame,
                         size_t *out_frame_len);

/* 应用对端的 offer 帧；成功后本机安装新 key 并自动 ack。 */
int32_t shio_sync_apply_offers(shio_kernel *handle,
                               const uint8_t *frame,
                               size_t frame_len);

/* 快照起点；用于日志已压缩时的重同步。 */
int32_t shio_sync_snapshot_start(shio_kernel *handle, uint64_t *out_at_seq);

/*
 * 读取一页快照并编码为 SHY2 帧。
 * after=NULL/0 表示第一页；out_next 是下一页的不透明游标；out_done=1 表示结束。
 * limit=0 使用默认 100，最大 1000。
 */
int32_t shio_sync_snapshot_page(shio_kernel *handle,
                                const uint8_t *after,
                                size_t after_len,
                                size_t limit,
                                uint8_t **out_frame,
                                size_t *out_frame_len,
                                uint8_t **out_next,
                                size_t *out_next_len,
                                uint8_t *out_done);

/* 应用快照帧；返回 {"applied":N,"skipped":N,"controls":N}。 */
int32_t shio_sync_apply_snapshot(shio_kernel *handle,
                                 const uint8_t *frame,
                                 size_t frame_len,
                                 uint8_t **out_report_json,
                                 size_t *out_report_len);

/* 本机发起轮换；返回 rotation_info JSON。 */
int32_t shio_sync_begin_rotation(shio_kernel *handle,
                                 uint8_t **out_json,
                                 size_t *out_json_len);

/* 当前轮换状态；没有进行中的轮换时返回 JSON null。 */
int32_t shio_sync_rotation_state(shio_kernel *handle,
                                 uint8_t **out_json,
                                 size_t *out_json_len);

/* 显式 ack 某个轮换目标 key。 */
int32_t shio_sync_ack_rotation(shio_kernel *handle, const uint8_t *to);

/* 把全部记录迁移到 to key；out_count 可为 NULL。 */
int32_t shio_sync_migrate_all(shio_kernel *handle,
                              const uint8_t *to,
                              uint64_t *out_count);

/*
 * 带限速参数的迁移；batch_size=0 使用默认 256，pause_ms=0 表示不暂停。
 * batch_size 上限 1024，pause_ms 上限 60000；取消在批次边界生效。
 */
int32_t shio_sync_migrate_all_ex(shio_kernel *handle,
                                 const uint8_t *to,
                                 uint32_t batch_size,
                                 uint32_t pause_ms);

/* 迁移进度 JSON：{"active":bool,"scanned":N,"migrated":N,"done":bool}。 */
int32_t shio_sync_migration_progress(shio_kernel *handle,
                                     uint8_t **out_json,
                                     size_t *out_json_len);

/* 请求取消迁移；在下一次批次边界生效。 */
int32_t shio_sync_cancel_migration(shio_kernel *handle);

/* 退役 from key。 */
int32_t shio_sync_retire(shio_kernel *handle, const uint8_t *from);

/* 清理 to key 的过期 offer。 */
int32_t shio_sync_prune_offers(shio_kernel *handle, const uint8_t *to);

/* 压缩 before 之前的变更日志；out_count 可为 NULL。 */
int32_t shio_sync_compact(shio_kernel *handle,
                          uint64_t before,
                          uint64_t *out_count);

/*
 * 生成配对包；ttl_seconds 仅用于调用方展示/过期控制，0 使用 600 秒；
 * 接收端固定按 10 分钟窗口校验 created_at。
 * out_frame 为 SHW2 外层帧，out_json 为 {"secret":"...","fingerprint":"..."}。
 */
int32_t shio_sync_create_pairing(shio_kernel *handle,
                                 uint64_t ttl_seconds,
                                 uint8_t **out_frame,
                                 size_t *out_frame_len,
                                 uint8_t **out_json,
                                 size_t *out_json_len);

/*
 * 用配对包创建新库；目标必须不存在或为空。
 * options_json 与 shio_open 相同；secret 为 Base32 文本字节。
 */
int32_t shio_open_with_pairing(const char *path,
                               const uint8_t *password,
                               size_t password_len,
                               const uint8_t *options_json,
                               size_t options_len,
                               const uint8_t *frame,
                               size_t frame_len,
                               const uint8_t *secret,
                               size_t secret_len,
                               shio_kernel **out_handle);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SHIO_H */
