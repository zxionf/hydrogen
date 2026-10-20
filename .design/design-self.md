# .design / .agent 目录设计规范

> [!NOTE]
> 此文件不允许ai修改

本文件描述项目设计文档目录 `.design/` 与 Agent 工作目录 `.agent/` 的组织方式、职责边界与更新机制。它是这两个目录本身的"设计真相源"，任何对目录结构的修改都应先更新本文件。

---

## 1. 设计目标

- **设计资产长期可读**：`.design/` 描述系统本身，项目结束后仍有价值，人类可独立读懂。
- **Agent 上下文可重建**：每次会话冷启动，不依赖聊天历史，按固定入口低成本装载上下文。
- **增量而非全量重写**：区分"稳定真相"与"增量记录"，前者修订、后者追加，避免双真相源和文档腐化。
- **设计与执行分离**：设计文档不被"下一步做什么""当前卡在哪"这类过程噪音污染。

---

## 2. 划分标准

判定规则：**项目结束后还有价值的、描述系统本身的，进 `.design/`；只对当前这轮工作有意义的、描述 Agent 怎么干活的，进 `.agent/`。**

| 内容 | 归属 | 理由 |
|---|---|---|
| 模块职责、接口契约、数据流、不变量 | `.design/SPEC.md` | 系统真相，长期有效 |
| 架构选型、权衡取舍的理由 | `.design/DECISIONS.log` | 设计历史，需要证据 |
| 本次变更改什么、影响面、兼容性 | `.design/changes/<id>/design.md` | 设计增量，合并后折叠进 SPEC |
| Agent 操作护栏、禁止事项、验证命令 | `.agent/CONTRACT.md` | 工作规则，不是设计 |
| 当前阶段、活跃变更、阻塞项 | `.agent/STATE.md` | 过程状态，易变 |
| 任务拆解与进度 | `.agent/work/<id>/tasks.md` | 执行过程，完成后归档 |
| 会话交接 | `.agent/work/<id>/handover.md` | 跨会话记忆，只保留最新 |
| 提示模板、常用命令、踩坑记录 | `.agent/prompts/`、`.agent/notes.md` | 提升 Agent 效率，非系统设计 |

硬约束：**`.design/` 里不写"下一步做什么"，`.agent/` 里不复制"系统是什么"。** 后者需要设计信息时只引用路径，不转述内容。

---

## 3. 目录结构

```
.design/
├── INDEX.md                    # 设计文档路由（人 + Agent 共用）
├── SPEC.md                     # 稳定层：当前设计真相源
├── DECISIONS.log               # 增量层：设计决策，append-only
├── changes/                    # 增量层：设计增量，按变更归档
│   └── <id>_<slug>/
│       └── design.md           # 改什么、为什么、影响面、迁移方式
└── (可选) diagrams/、adr/      # 架构图、独立 ADR 等设计产物

.agent/
├── INDEX.md                    # Agent 入口路由，先读这个
├── CONTRACT.md                 # 操作契约与硬约束
├── STATE.md                    # 当前状态指针（极短）
├── work/
│   └── <id>/                   # 与 .design/changes/<id> 同 ID
│       ├── tasks.md            # 执行步骤与进度
│       └── handover.md         # 会话交接，每次结束覆写
├── prompts/                    # 可复用提示模板
└── notes.md                    # 踩坑、命令、工具偏好（append-only）
```

根目录 `AGENTS.md` 只写一行指路：`先读 .agent/INDEX.md，设计上下文按其中指引取 .design/`，不重复内容。

`.design/` 与 `.agent/` 通过**同一个 `<id>` 关联**：`.design/changes/<id>_<slug>/design.md` 说"改什么"，`.agent/work/<id>/tasks.md` 说"怎么做、做到哪"。一个变更一个 ID，两侧各自演进，合并时一起归档。

---

## 4. 文件职责与更新方式

### 4.1 `.design/` 侧

**`INDEX.md` — 路由表。**
列 `文件 | 一句话 | 何时读`。Agent 需要设计上下文时通过这张表按需取，而不是遍历整目录。新增设计文档时必须同步登记。

**`SPEC.md` — 修订式。**
只写当前有效事实，一个概念一节，保持短小。更新触发是"一次变更被接受并合并"：把 `changes/<id>/design.md` 的结论**折叠进对应章节**，而不是追加补丁段。历史交给 git，文件内不堆叠版本。

**`DECISIONS.log` — 只追加。**
固定三段：Context → Decision → Consequences。被推翻的决策标 `superseded by #N`，不重写、不删除。写入义务挂在"任何非显然的设计选择发生时"，由 Agent 当场追加。这是设计侧唯一允许频繁增长的文件。

**`changes/<id>_<slug>/design.md` — 变更期间修订，合并后归档。**
内容限于设计增量：接口变化、数据模型调整、兼容性与迁移、对现有不变量的影响。**不写任务清单和进度**。

### 4.2 `.agent/` 侧

**`INDEX.md` — Agent 入口路由。**
Agent 启动时第一个读的文件，指向 `STATE.md`、当前 `work/<id>/` 以及 `.design/INDEX.md`。

**`CONTRACT.md` — 极少改。**
技术栈锁定、目录边界、禁止触碰的模块、验证命令、完成定义，以及元规则："任何非显然设计选择必须当场写入 `.design/DECISIONS.log`，否则会话不算完成。"修改本文件本身需在 `DECISIONS.log` 留一条。

**`STATE.md` — 频繁更新但极短。**
三到五行：当前阶段、活跃 `<id>`、阻塞项、下一条该读的文件。它是指针，不是状态大全；内容变长即说明在越权承载 `SPEC.md` 或 `DECISIONS.log` 的信息。

**`work/<id>/tasks.md` — 随执行实时更新。**
步骤 + 状态。只引用设计，不复述设计内容。

**`work/<id>/handover.md` — 每次会话结束覆写。**
内容：已完成什么、没做完什么、关键选择及理由、下一步从哪续。只保留最新交接，历史靠 git。新会话装载顺序：`.agent/INDEX.md` → `STATE.md` → `work/<id>/handover.md` → `.design/SPEC.md` 相关章节。

**`notes.md` / `prompts/` — 只追加。**
工具用法、踩坑、可复用指令模板。属于 Agent 效率资产，与系统设计无关，禁止放入 `.design/`。

---

## 5. 更新机制

把更新挂到事件上，而非依赖自觉。每个事件明确"写哪个文件夹的哪个文件"：

| 事件 | 写入 `.design/` | 写入 `.agent/` |
|---|---|---|
| 会话开始 | 只读（按 INDEX 取章节） | 读 INDEX → STATE → handover |
| 做出设计选择 | **追加** `DECISIONS.log` | — |
| 新需求/修复开始 | 创建 `changes/<id>/design.md` | 创建 `work/<id>/tasks.md`，更新 `STATE.md` |
| 执行推进 | — | 更新 `tasks.md` |
| 会话结束 | — | 覆写 `STATE.md` 和 `handover.md` |
| 变更合并 | `design.md` 结论**折叠进** `SPEC.md`，归档 change | 归档 `work/<id>`，清 `STATE.md` |

概括：**设计侧"追加 → 折叠"，Agent 侧"指针 → 覆写"。** `.design/` 的增长受控（决策只追加、变更合并即折叠），`.agent/` 是循环复用的工作区。

---

## 6. 防漂移的机械约束

用 git hook 或 CI 兜底，不依赖 Agent 自觉：

- `.design/DECISIONS.log` 出现删除行 → 拒绝合并。
- `.design/changes/` 有新合并的 change 但 `SPEC.md` 未变 → 提醒折叠。
- `.agent/work/<id>/tasks.md` 引用了不存在的 `.design/changes/<id>` → 报错，保证 ID 双向可查。
- `STATE.md` 超过约定行数 → 提醒瘦身。

---

## 7. 最小启动顺序

从零开始时不必一次建全，按以下顺序落地：

1. 建 `.agent/INDEX.md` + `.agent/CONTRACT.md`：让 Agent 知道入口和护栏。
2. 建 `.design/INDEX.md` + `.design/SPEC.md`，写当前系统骨架事实，不求全。
3. 建 `.design/DECISIONS.log`，从下一个真实设计选择开始追加，不补历史。
4. 第一个真实变更到来时，同时建 `.design/changes/<id>/design.md` 和 `.agent/work/<id>/tasks.md`；`STATE.md` 在首次会话结束时写入。

---

## 8. 不变式

以下三条在任何演进中都必须成立：

1. **单一真相源**：系统的"当前是什么"只存在于 `.design/SPEC.md`，其他文件只能引用，不能复制。
2. **决策只追加**：`.design/DECISIONS.log` 永不删改，理由的可追溯性是其存在意义。
3. **`.design/` 独立可读**：删除整个 `.agent/` 后，`.design/` 仍应能被人类完整理解，不依赖任何 Agent 过程信息。
