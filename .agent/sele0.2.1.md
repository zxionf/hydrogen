# sele CLI 0.2.1

一个用于管理 `.design/` 与 `.agent/` 目录的 C++20 命令行工具，支撑「创建变更 → 追加决策 → 查询状态 → 规则检查 → 归档预览」这一最小闭环。

## 命令

所有命令均支持以下全局选项：

- `--root <path>` — 项目根目录（默认：当前目录，并向上查找 `.design/`）
- `--output text|json` — 输出格式（默认：text）

### 输出格式

`--output text`（默认）面向人，打印一段摘要；`--output json` 面向程序，字段固定。两者由同一份数据渲染，内容一致。

```text
$ sele status
Active change: 20261020-my-feature
Tasks: 2/5 done
Decisions: 3
Consistent: yes
```

```text
$ sele lint
Lint failed
  [ok] id_consistency
  [skip] decisions_append_only
  [fail] spec_process_words
  [ok] dir_integrity
```

其余命令的 text 形态：`init` 输出 `Initialized ...` 加 design/agent 路径，`new` 输出 `Created <id>` 加两侧路径，`append-decision` 输出 `Appended decision #<n> -> <path>`，`version` 输出 `sele <版本>` 加两行依赖版本；`close --dry-run` 输出 `Close preview for <id> ...` 加折叠计划，实际关闭输出 `Closed <id> (spec folded, directories kept)` 加每个小节的 action 与指针是否复位。

### `sele version [--output json]`

查看版本。不需要 `.design/`，任意目录可运行；`--version` 与它同源。

```bash
sele version
sele version --output json
sele --version                 # 单行：sele 0.2.0 (CLI11 2.4.2, nlohmann/json 3.11.2)
```

```json
{"name":"sele","version":"0.2.0","cli11":"2.4.2","nlohmann_json":"3.11.2"}
```

退出码：0=成功

### `sele init [directory]`

初始化项目，创建 `.design/`、`.agent/` 目录及其模板文件（INDEX.md、SPEC.md、DECISIONS.log、CONTRACT.md、STATE.md 等）。

```bash
sele init                 # 默认使用当前工作目录
sele init ./my-project    # 指定目录（位置参数）
sele init --root ./my-project  # 也可用 --root 指定
```

- 若目标目录已存在 `.design/` 或 `.agent/`，则报错退出（不会覆盖）。
- 退出码：0=成功，2=已初始化，4=环境错误

### `sele new --slug <name>`

创建一次新的变更，同时生成 design 与 work 目录。

> 必须先执行 `sele init` 完成初始化，否则 `new` 会报错并提示先初始化。

```bash
sele new --slug my-feature
```

退出码：0=成功，1=slug 非法，2=ID 已存在，4=未初始化或环境错误

### `sele append-decision --context <c> --decision <d> --consequences <e> [--change-id <id>]`

向 `.design/DECISIONS.log` 追加一条决策记录。

```bash
sele append-decision --context "选用 C++20" --decision "使用 modules" --consequences "导入更简洁" --change-id 20261018-my-feature
```

退出码：0=成功，1=缺少参数，4=环境错误

### `sele status [--output json]`

查看当前活动变更的状态（只读、快速）。

```bash
sele status --output json
```

退出码：0=成功，4=环境错误

### `sele lint [--fix]`

运行机械化的规则检查：

1. ID 双向一致性（`.design/changes/<id>` 与 `.agent/work/<id>` 互相对应）
2. DECISIONS.log 只允许追加（通过 git diff 判断）
3. SPEC.md 不得包含过程性词汇（TODO、WIP、待办、下一步、进行中）
4. 目录完整性（design.md、tasks.md 是否存在）

```bash
sele lint
sele lint --fix  # 自动补齐缺失的模板文件
```

退出码：0=全部通过，3=存在检查失败，4=环境错误

### `sele close <id> [--dry-run]`

关闭一次变更：把 `.design/changes/<id>/design.md` 的各 `##` 小节折叠进 `.design/SPEC.md`。

- 命中 SPEC.md 已有同名（互为子串）小节 → **替换该小节正文**（修订式折叠，不追加补丁段）。
- 未命中 → 在 SPEC.md 末尾**追加新小节**；同一份 design.md 内重复的标题合并到同一小节。
- 正文为空或只含 HTML 注释的小节跳过，不写入 SPEC.md；注释后面还有正文的按内容处理。
- fenced code 块里的 `## ` 行不算小节，代码块整体原样保留。
- `.design/changes/<id>/` 与 `.agent/work/<id>/` **原地保留**，历史交给 git。
- 仅当 `STATE.md` 的 `Active Change:` 正是该 id 时复位为 `无`；指向其他变更时保持不变。
- 折叠结果与折叠前一致时不重写 SPEC.md（JSON `spec_updated` 为 `false`），重复关闭不会产生无意义的 diff。

```bash
sele close 20261018-my-feature --dry-run   # 只预览折叠计划，不写任何文件
sele close 20261018-my-feature             # 执行折叠并复位指针
```

`--dry-run` 输出真实解析出的小节（不再用写死的标题），匹配不到时映射值为空字符串；计划用 `replaced` / `appended` / `skipped` 一套动作词，文本与 JSON 的 `plan` 字段一致。

退出码：0=成功，1=缺少参数，2=两侧目录或 design.md 缺失，4=环境错误

## 退出码

| 代码 | 含义 |
|------|------|
| 0 | 成功 |
| 1 | 用法错误（缺少或非法参数） |
| 2 | 状态冲突（ID 已存在、ID 不存在） |
| 3 | lint 检查未通过 |
| 4 | 环境错误（目录不存在、git 不可用、权限不足） |

## JSON 输出

每个命令都支持 `--output json`，字段名稳定，可被 `jq` 解析。

示例：

```json
{"id":"20261018-my-feature","design_path":".design/changes/.../design.md","work_path":".agent/work/.../tasks.md","state_updated":true}
```
