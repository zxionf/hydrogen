# 20261024-storage-maintenance

<!-- 存储维护：先做轮换进度/限速，再做同设备备份/恢复；导入导出与多设备克隆后置。 -->

## 存储维护范围

存储维护拆成两个独立阶段，按 S1 → S2 推进：

- **S1 轮换进度与限速**：只覆盖 `format=2` 同步库的 `migrate_all`。提供批次大小、批次间暂停、进度查询与取消；迁移仍在后台线程阻塞执行。
- **S2 同设备备份与恢复**：对已打开的库做一致性快照克隆；恢复只用于同一设备/同一安装的灾难恢复。

不在本次范围：

- 分步迁移 API（方案 B）
- 原始数据导出/导入
- 多设备备份、克隆或迁移
- 物理文件压缩、自动日志压缩
- Android SAF/文件描述符接口

## 轮换进度与限速

当前 `migrate_all(to)` 在持有 session 写锁的情况下按 256 条一批重加密，调用期间普通读写都会被阻塞；没有进度、取消和限速。S1 采用方案 A：保留后台阻塞迁移的语义，增加可控参数与旁路观察接口。

### 执行模型

- 调用方在后台线程执行迁移，UI 线程只轮询进度。
- 内核提供 `batch_size` 与 `pause_ms`：
  - `batch_size` 控制每批记录数，默认保持 256。
  - `pause_ms` 控制每批提交后的暂停时间，默认 0；用于降低 CPU/IO 占空比，不做强制限速。
- 迁移仍持有 session 写锁；迁移期间普通读写返回等待，进度查询不得获取该锁。
- 取消在批次边界生效：当前批提交后停止，已迁移记录保留，旧 key 不删除；下次迁移从起点重新扫描并跳过已迁移记录。
- 进度只报告 `active`、`scanned`、`migrated`、`done`，不承诺百分比；如需百分比，后续单独增加总数统计。

### 恢复语义

- 恢复中断的 `format=2` 轮换时，先调用 `rotation_state()` 取得当前 `to`，再调用带进度的 `migrate_all(to)`。
- `rotate_data_key()` 不再作为恢复入口；它只用于发起一次新的“开始轮换 + 迁移”便捷流程。
- `format=1` 轮换不增加进度 API；旧库仍可打开，并继续使用既有轮换路径。

### C ABI 形态

新增或调整轮换相关入口，保持参数显式：

```c
int32_t shio_sync_migrate_all_ex(shio_kernel *handle,
                                 const uint8_t to[16],
                                 uint32_t batch_size,
                                 uint32_t pause_ms);

int32_t shio_sync_migration_progress(shio_kernel *handle,
                                     uint8_t **out_json,
                                     size_t *out_len);

int32_t shio_sync_cancel_migration(shio_kernel *handle);
```

- 进度 JSON 字段：`active`、`scanned`、`migrated`、`done`。
- 取消后返回明确的取消错误码；不在批次中间丢弃已提交事务。
- 轮换相关 C ABI 不承诺与旧客户端兼容；其他非轮换接口仍按增量方式演进。
- `batch_size` 限定在合理上限内，`pause_ms` 设置上限；非法值返回参数错误。

## 备份与恢复

S2 只做同设备备份/恢复，先解决灾难恢复与迁移前保护，不把“导出”与“备份”混为一谈。

### 备份

- 对已打开的库执行在线一致性快照克隆：
  - 在一个 redb 读事务中读取所有表：`meta`、`keys`/`keys2`、`catalog`、`records`、`changes`、`sync`。
  - 将原始键值写入新的 redb 文件，不解密、不重加密。
  - 备份文件仍是正常的 shio 数据库，使用原库口令打开。
- 备份保留 `db_id`、`device_id`、key 链、同步日志和 peer 游标；因此只适用于同一设备/同一安装的恢复。
- 目标路径必须不存在；先写临时文件，完成并校验结构后再原子改名。
- 长读事务会阻止 redb 回收旧页，源库在备份期间可能暂时变大；这是在线备份的已知代价。
- 备份默认不压缩同步日志；如需压缩，调用方先显式执行 `compact_changes`，并自行承担对端可能要求重同步的后果。

### 恢复

- 恢复目标必须不存在或为空；恢复过程不覆盖正在使用的库。
- 恢复后必须用原口令打开并完成结构校验；校验失败则删除目标，不留下半成品。
- 恢复出的库与原库具有相同 `device_id`。禁止把同一份备份恢复到两台设备并同时启用同步；多设备场景继续使用配对流程。
- 备份/恢复接口先只接受文件路径；Android 的 SAF/文件描述符由应用层复制到私有目录后再调用内核。

### C ABI 形态

```c
int32_t shio_backup(shio_kernel *handle,
                    const char *out_path,
                    uint8_t **out_json,
                    size_t *out_json_len);

int32_t shio_restore(const char *backup_path,
                     const char *target_path,
                     const uint8_t *password,
                     size_t password_len,
                     const uint8_t *options_json,
                     size_t options_len,
                     shio_kernel **out_handle);
```

- `shio_backup` 的摘要 JSON 字段为 `format_version`、`db_bytes`、`table_count`、`row_count`；不携带明文或密钥。
- 备份先写同目录临时文件，校验全部已知表的行数和 `meta` 原始字节后再原子改名；失败时删除临时文件，目标路径保持不存在。
- `shio_restore` 返回已打开的目标句柄，减少“恢复成功后再次打开”的窗口。
- `shio_restore` 接受不存在的目标或 0 字节文件；其他已存在目标返回参数错误。`options_json` 的 `create` 被忽略，恢复始终不以 create 语义重新初始化数据库。
- 恢复先复制到目标同目录临时文件，用原口令打开并完成 key check/结构校验，再替换空目标；失败时删除临时文件，不留下半成品。
- 非空目标、已存在备份目标、错误口令和损坏备份分别按现有错误映射返回；不新增专用错误码。
- 备份/恢复只保留同设备身份信息，不生成新的 `device_id`，也不清理同步游标。

## 兼容性与版本

- 内核包版本为 `0.2.0`；C ABI 版本保持 `SHIO_ABI_VERSION=1`，新增轮换符号为增量扩展。
- 轮换接口是兼容性例外：允许不兼容旧版轮换客户端；旧数据库的读取与既有轮换路径不变。
- 非轮换 C ABI 继续保持增量演进，不因 S1/S2 破坏现有调用。
- `format=1` 与 `format=2` 的记录布局不变；S2 的备份文件直接复用对应格式。

## 后续计划

以下内容写入后续计划，不进入 S1/S2：

- 方案 B：分步迁移 API，支持持久化 cursor、维护模式与完全合作式调度。
- 物理文件压缩（redb `Database::compact()`）与自动日志压缩策略。
- 多设备备份/克隆：必须生成新的 `device_id`，清理 peer 状态，避免重复身份同步。
- 原始数据导出/导入：格式以后约定；默认输出原始数据/明文，绕过记录加密，属于危险操作，必须显式警告、二次确认，不能作为默认备份路径。
- 明文导出工具：独立工具，不进入内核默认 API。
