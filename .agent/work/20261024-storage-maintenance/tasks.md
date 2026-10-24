# 20261024-storage-maintenance 任务清单

> 顺序固定：先 S1 轮换进度与限速，再 S2 同设备备份与恢复。S2 不提前实现导入导出。

## S1：轮换进度与限速

- [x] core：定义迁移参数与进度类型，校验 `batch_size`、`pause_ms` 上限
- [x] core：为 `format=2` 的 `migrate_all` 增加批次间暂停、进度计数与批次边界取消
- [x] core：进度查询走旁路状态，不获取 session 写锁；迁移期间读写阻塞语义保持不变
- [x] core：取消后保留已迁移记录与旧 key；持久化游标从上次提交点续跑（方案 B 第一步）
- [x] core：明确恢复入口为 `rotation_state()` + `migrate_all(to)`；`rotate_data_key()` 不再承担中断恢复
- [x] core：`format=1` 不增加进度能力，既有轮换路径回归
- [x] ffi：新增 `shio_sync_migrate_all_ex`、`shio_sync_migration_progress`、`shio_sync_cancel_migration`
- [x] ffi：补充取消错误码与进度 JSON 映射，更新 `shio.h`
- [x] tests：正常完成、暂停参数、取消后续跑、非法参数、迁移期间读写等待、重复轮换
- [x] android：C harness 增加进度/取消场景，x86_64 模拟器通过
- [x] docs：实现后更新 `api.md`、`SPEC.md`、`notes.md`

## S2：同设备备份与恢复

- [x] core：在线快照克隆，单读事务复制全部表到新的 redb 文件
- [x] core：目标不存在校验、临时文件、结构校验、原子改名、失败清理
- [x] core：恢复目标不存在或为空；恢复后用原口令打开并校验，失败删除目标
- [x] core：备份保留 `db_id`、`device_id`、key 链、同步日志与 peer 游标
- [x] ffi：新增 `shio_backup` 与 `shio_restore`，更新 `shio.h`
- [x] tests：`format=1`/`format=2` 备份恢复、在线写入期间备份、错误口令、目标已存在、损坏备份、同步身份保持
- [x] tests：恢复后与原库不可同时作为同一设备参与同步的约束有文档与错误防护
- [x] android：C harness 增加路径备份/恢复场景，x86_64 模拟器通过
- [x] docs：实现后更新 `api.md`、`data.md`、`SPEC.md`、`notes.md`

## 后续计划

- [ ] 方案 B：分步迁移 API（持久化 cursor 已完成；维护模式、合作式调度待做）
- [ ] 物理文件压缩：接入 redb `Database::compact()`，需要关闭句柄或独占维护接口
- [ ] 自动日志压缩策略；当前仍只手动 `compact_changes`
- [ ] 多设备备份/克隆：新 `device_id`、清理 peer 状态、防止重复身份同步
- [ ] 原始数据导出/导入格式约定；必须标为危险操作并二次确认
- [ ] 明文导出工具；独立工具，不进入内核默认 API
