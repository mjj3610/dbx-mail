# 实施计划：MCP 工具面（`mcp/tools` + `mcp/call`）

> 分支：feat/m365-outlook-graph。目标：把 Sidecar 已有的邮件能力（列表 /
> 阅读 / 旗标 / 移动 / 发送 / 文件夹）按 DBX Plugin Protocol v1 的 MCP 约定
> 暴露给内置 AI Agent 与外部 `dbx` MCP 服务。
>
> 核心原则：**纯发现 + 严格调用**。`mcp/tools` 无副作用（不联网、不开监听、
> 不写库）；`mcp/call` 逐参数校验、未知工具报错、凭据与密码绝不进参数与输出、
> 写操作显式且遵循 `sidecar-protocol.md` §9 的失败/事务语义。

## 0. 背景与结论

- 契约来源（已核对）：
  - 宿主 schema `host/plugins/manifest.schema.json` 的 `mcpContribution`：
    合法字段只有 `type` / `id` / `description` / `ai_tools`（默认 true）/
    `external_tools`（默认 false）/ `toolsets[]`（`id` 需匹配
    `^[a-z0-9][a-z0-9._-]*$`）/ `tools`（键为工具名，值为
    `{toolset, access: read|write|full}`）；`additionalProperties: false`。
    `external_tools: true` 时**必须**逐工具声明 access，否则不进外部 MCP 面。
  - sidecar-protocol §9：`mcp/tools` 收 `{connectionId}` 返回
    `{tools:[{name, description, inputSchema, annotations?}]}`；`mcp/call` 收
    `{tool, arguments, lifecycle}` 返回 MCP `CallToolResult`
    （`{content:[{type:"text",text}], isError}`）；inputSchema 只用宿主可移植
    子集（type/description/properties/required/items/enum/边界）；单次调用
    120 秒上限；仅 `annotations.readOnlyHint: true` 可直接执行。
  - 兄弟插件 `ssh` 的真实样例：manifest 用
    `{type:"mcp", id, description, external_tools:true, toolsets, tools}`，
    后端 `mcp.rs` 提供 `tool_definitions()` 与 `call_dbx()`。
- 本插件事实：邮件账户与缓存都在插件自己的 SQLite（`db.rs`），MCP 工具以
  插件自有 `accountId` 为作用域，不依赖宿主连接绑定；`mail/list`（offset
  分页 + `ThreadFilter`）、`sync::ensure_body`（按需取正文）、
  `sync::set_flags` / `sync::move_messages`（本地先写 + 待同步队列）、
  `compose::send`（发件箱队列 + 撤销窗口）均可直接复用，不复制代码。
- i18n 仅 zh-CN / en；工具输出面向模型，只给事实，不夹带指令。

## 1. Manifest 贡献点

在 `contributions` 追加（其余内容不动）：

```json
{
  "type": "mcp",
  "id": "com.mjj3610.mail.mcp",
  "description": "Mail tools over the plugin's own accounts: list, read, flag, move and send messages.",
  "external_tools": true,
  "toolsets": [
    { "id": "mailbox-read", "title": "Reading mail" },
    { "id": "mailbox-write", "title": "Mail actions" }
  ],
  "tools": { … 每个工具声明 toolset + access … }
}
```

- `external_tools: true` 的理由：宿主默认 false，但 ssh 样例与外部 `dbx`
  MCP 场景均按 true 落地；邮件工具作用于插件自有账户，外部暴露受宿主全局
  MCP 工具白名单 + 连接白名单双重闸门约束，且本插件逐工具声明了 access
  （读=read，写=write），外部策略可单独关掉写工具。若宿主校验要求收紧，
  去掉该字段即回退默认 false，不影响内置 AI 面。
- `ai_tools` 缺省（true）：内置 Agent 在打开的工作台连接上可用。
- 每个 Manifest 只允许一个 `mcp` 贡献点；已有 `entrypoints.backend` 满足要求。

## 2. 工具清单（snake_case，6 个）

| 工具 | toolset | access | readOnlyHint | 语义 |
| --- | --- | --- | --- | --- |
| `folder_list` | mailbox-read | read | true | 账户与文件夹清单（含 role/unread/total） |
| `mail_list` | mailbox-read | read | true | 会话列表：`folder`（role）/`folderId`/`query`/游标分页 |
| `mail_read` | mailbox-read | read | true | 纯文本会话内容（逐封 From/Date + 正文，正文未缓存时可联网补取） |
| `mail_flag` | mailbox-write | write | — | 写：seen / flagged / answered（单封或整会话） |
| `mail_move` | mailbox-write | write | — | 写：archive / trash / junk / 指定 folderId（单封或整会话） |
| `mail_send` | mailbox-write | write | — | 写：两阶段发送，`dryRun:true` 只预览不排队 |

参数要点（camelCase 字段，与 sidecar 协议一致）：

- `accountId`：插件自有账户 id；`mail_send` 必填，其余缺省 0 = 全部账户。
- `mail_list` 游标：`cursor` 为不透明字符串（实现为 `o<offset>`，非法值报
  参数错误），`limit` 缺省 20、上限 50；有下一页时返回 `nextCursor`。
- `mail_read`：`threadId`（来自 mail_list）必填；`network` 缺省 true，正文
  未缓存时按需取原始邮件并解析为纯文本（复用 `sync::ensure_body`）；读取
  **不**改动已读状态（纯只读，与 UI 的 `mail/open` 不同）。
- `mail_flag` / `mail_move`：`messageId` 或 `threadId` 二选一（都给以
  messageId 为准）；flag 至少给一个 `seen`/`flagged`/`answered`；move 的
  `destination` ∈ `archive|trash|junk` 或给 `folderId`。写本地后按现有
  pending 队列后台推远端，返回 `{ok:true, queued:<n>}`，网络失败不回滚本地
  （deferred sync，§9 语义：如实返回已排队，不宣称已同步）。
- `mail_send`：`to` 必填（数组），`cc`/`bcc`/`subject`/`html` 可选，
  `dryRun` 缺省 false。dryRun 校验账户与收件人后返回完整预览
  （from/to/cc/bcc/subject/正文预览/附件提示/undoSeconds），**不写发件箱、
  不联网**；真实发送复用 `compose::send` 默认路径（带撤销窗口，可从 UI 撤
  销），返回 `{queued:true, outboxId, scheduledAt, undoSeconds}`——如实
  报告「已排队」而非「已发送」（§9：投递是异步的，不夸大结果）。

## 3. `mcp/call` 安全与错误语义

- 入参：`tool`（字符串，必填）、`arguments`（对象，可缺省为 `{}`）、
  `lifecycle`（可选）。未知工具 → 参数错误（-32602）；`arguments` 非对象 →
  参数错误；未声明键一律拒绝（白名单外键报参数错误），宿主可能注入的
  `connectionId` 只接受不透传。
- 类型校验逐字段进行：字符串非空、整数范围、数组元素、枚举值非法即报
  -32602，错误信息可读（中文，与仓库现有错误风格一致）。
- 凭据红线：工具参数 schema 与输出都只含邮件数据；`folder_list` 输出账户
  邮箱地址与显示名（非凭据），密码/token/reference 一律不出现。
- lifecycle：存在时校验为对象并记录来源（供将来按宿主连接过滤），当前工具
  语义不依赖它；不存在时按插件自有账户处理（内置 Agent 绑定已打开连接，
  外部 MCP 绑定保存配置，均由宿主负责注入）。
- 失败语义（§9）：本地写成功 + 远端推送失败 → 返回 queued 状态 + 错误计数，
  不自动重试、不宣称回滚；`mail_read` 网络补取失败 → 返回已缓存部分 +
  `bodyPending` 标记；单工具输出体积受 limit/snippet 截断约束，不做分块流。

## 4. 代码落点

- `backend/src/mcp.rs`（新）：`tool_definitions() -> Value`（纯静态发现）、
  `call(app: &Arc<App>, params: &Value) -> RpcResult`（分发 + 严格校验 +
  复用 accounts/sync/compose/db）。`#[cfg(test)]` 单测覆盖：
  1. tools 列表形状：6 个工具、命名 snake_case、readOnlyHint 只在读工具、
     inputSchema 均为 object 且 required 合法、不含 password/token 字段；
  2. 参数校验失败路径：未知工具、未知参数键、缺 to/旗标、坏 cursor、
     destination 非法均报错；
  3. `mail_send` dryRun：真实 App（临时目录 + 内存态密钥/运行时）上执行
     预览后断言发件箱仍为空、返回预览内容完整。
- `backend/src/main.rs`：`dispatch` 增加 `"mcp/tools"` 与 `"mcp/call"`
  两个分支；`mod mcp;` 声明。
- `scripts/sidecar-check.mjs`：新增「MCP 工具面」节——tools 形状、未知工具
  报错、dry_run 预览且发件箱为空、空库 folder_list；发送预览用例仅在存在
  测试账户（debug Graph 节）时执行，release 构建按现有约定 SKIP 不 FAIL。
- 文档：README 协议段补 `mcp/tools` / `mcp/call` 与工具表；
  `docs/版本部署变更说明-v0.2.0.md` 追加独立 MCP 小节。

## 5. 任务清单

- [x] 阅读契约（skill contributions §8 / sidecar-protocol §9 / 宿主 schema / ssh 样例）
- [x] `docs/IMPL_PLAN_MCP.zh-CN.md`（本文档）
- [x] `manifest.json` 追加 mcp 贡献点（不动其余内容）
- [x] `backend/src/mcp.rs`：tool_definitions + call + 6 工具 + 单测
- [x] `backend/src/main.rs`：`mod mcp` + 两个分发分支
- [x] `scripts/sidecar-check.mjs`：MCP 冒烟节（发现/未知工具/dry_run/校验失败）
- [x] `README.md` 协议段 + `docs/版本部署变更说明-v0.2.0.md` MCP 小节
- [x] `cd backend && cargo test` 全绿
- [x] `node scripts/sidecar-check.mjs` MCP 用例全绿

## 6. 非目标

- 不做 MCP 会话型工具（无 start/stop 长连接语义；邮件操作皆单次往返）。
- 不在 `mcp/tools` 阶段联网探测账户可达性（纯本地 SQLite 读取）。
- 不新增 crate；不动前端（无需 tsc 验证）。
- 不处理宿主连接级 scope（`scope` 字段）——邮件账户是插件自有数据，
  无「连接下的子作用域」概念。
