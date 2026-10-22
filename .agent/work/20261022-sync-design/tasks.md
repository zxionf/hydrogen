# 20261022-sync-design 任务清单

- [x] 定同步模型：`db_id` / `device_id` / 本地日志 / 多跳传播
- [x] 定 format 2：meta/key/record 的 128 位 key_id 布局与 v2 AAD
- [x] 定可恢复升级：format 1→2、混合解码、旧二进制拦截、升级期间禁改口令/轮换
- [x] 定表与字节格式：`shio.changes`、`shio.sync`、`shio.keys2`、ChangeBlob、帧、配对包
- [x] 定任意设备轮换：随机 key id、offer/ack/公告/迁移公告、DAG 安装、写 key 收敛
- [x] 定拉取、快照、压缩、记录冲突全序与失败恢复
- [x] 定 core / sync / facade 接口、错误变体、测试计划与里程碑
- [x] 确认不可回退升级、`(created_at, key_id)` 收敛顺序、只读降级工具排期
- [x] M0：format 2 编解码、AAD、混合读取、`enable_sync` 升级状态机与旧格式拦截
- [x] M1：`shio.changes` / `shio.sync`、写日志、`changes_since`、`apply_page`、记录 LWW 与两副本收敛
- [x] M2：`snapshot_start/page`、`compact_changes`、`ResyncRequired` 与三副本/快照测试
- [x] M3：随机 `key_id` 轮换、offer/ack/公告、DAG 安装、写 key 收敛、`migrate_all` / `retire` / `prune`
- [x] M4：配对包 `SHB2` / `SHW2`、密钥链、指纹、基线与配对测试
- [x] M5：`shio-sync` 帧包装、facade `sync` feature、示例与 `SPEC`/`api`/`data` 折入
