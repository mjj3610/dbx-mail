# 实施计划：Microsoft 365 / Outlook.com（Graph API）支持

> 版本目标：0.2.0。技术决策（用户已确认）：**Microsoft Graph API 传输** +
> **用户自填 Azure client_id / tenant** + **授权码 + PKCE 本地回环回调** +
> **收发全量对齐现有 IMAP 账户**。
>
> 核心原则：**传输层替换、缓存层不动**。Graph 同步把邮件按现有 `mailboxes /
> messages / bodies / attachments` 的形状灌入 SQLite，会话归并、搜索、阅读器、
> 附件、导出、草稿、发件箱等下游功能零改动。

## 0. 背景与结论

- 现有 `outlook` 预设指向 `outlook.office365.com`，但微软已对 M365 /
  Outlook.com 停用 IMAP/SMTP 基本认证，纯密码登录不可用。
- `backend/src/imap.rs` 只有 `LOGIN`；引入 OAuth XOAUTH2 是备选路线，本次按
  用户决策直接走 Graph REST，不再依赖租户是否开启 IMAP。
- 已核实的仓库事实（实现前提）：
  - `smtp::build(&Outgoing) -> Vec<u8>` 可独立产出 MIME（与 SMTP 发送解耦），
    Graph 发送直接复用，`Message-ID / References / In-Reply-To` 头得以保留，
    会话线程不断。
  - `mail/list` 分页按 `ORDER BY m.date DESC, m.id DESC`，不依赖 uid 游标；
    uid 仅是 IMAP 的唯一键成分。
  - 草稿仅存本地 SQLite（不推服务器 Drafts），Graph 账户天然对齐。
  - 原始邮件以 `raw_path` 文件落盘，header 解析统一走 `mime.rs` /
    `mail-parser`；Graph 取 `GET /messages/{id}/$value`（完整 MIME）后可复用
    100% 现有解析管线。
  - `reqwest(rustls) / sha2 / base64 / urlencoding / chrono / uuid` 已在
    `Cargo.toml`，**不新增任何 crate**。
  - `net_guard` 只管出网校验，`127.0.0.1` 回环监听不冲突；tokio 已在运行时。
  - i18n 为 zh-CN / en 双语；前端 `rpc.ts` 支持逐调用超时，OAuth 用轮询。

## 1. Azure AD 应用注册（最终用户操作，写入文档与向导提示）

1. 登录 https://portal.azure.com → Microsoft Entra ID → 应用注册 → 新注册。
2. 名称任意（如 `DBX Mail`）；受支持账户类型选「任何组织目录中的账户和个人
   Microsoft 账户」（多租户 + 个人，对应 tenant `common`）。
3. 重定向 URI：平台选「移动和桌面应用程序」，填 `http://localhost`
   （注册时不带端口；sidecar 用任意回环端口均可匹配）。
4. 「API 权限」→ Microsoft Graph → 委托权限：
   `offline_access`、`Mail.ReadWrite`、`Mail.Send`、`User.Read`。
   受限租户需管理员同意（向导与文档注明）。
5. 概述页复制「应用程序(客户端) ID」填入插件；租户 ID 可填 `common`
   （推荐默认）、`organizations`、`consumers` 或具体租户 GUID。

## 2. 协议变更（camelCase，沿用 `域/动作` 命名）

新增方法：

| 方法 | 参数 | 返回 |
| --- | --- | --- |
| `account/oauthBegin` | `{tenantId, clientId}` | `{authorizeUrl, state, expiresIn}` |
| `account/oauthStatus` | `{state}` | `{status: pending\|done\|failed, email?, error?}` |
| `account/oauthCancel` | `{state}` | `{ok: true}` |

变更方法：

- `account/presets`：新增 `provider: "graph"` 预设 `msgraph`；
  `outlook` 预设 hint 改为提示基本认证停用、建议走 Graph。
- `account/add`：接受 `provider: "graph"`、`tenantId`、`clientId`、
  `oauthState`（认领 pending token）；graph 账户不传密码。
- `account/update`：允许更新 `tenantId` / `clientId`；graph 账户跳过密码分支。
- `account/test`：graph 账户改为「刷新 token + GET /me」验证。
- `sync/now`、`mail/flags`、`mail/move`、`compose/send` 内部按 provider 分流，
  协议形状不变。

## 3. 数据库迁移（user_version 1 → 2）

```sql
ALTER TABLE accounts ADD COLUMN provider  TEXT NOT NULL DEFAULT 'imap';  -- imap | graph
ALTER TABLE accounts ADD COLUMN auth_type TEXT NOT NULL DEFAULT 'password'; -- password | oauth2
ALTER TABLE accounts ADD COLUMN tenant_id TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN client_id TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN remote_id TEXT NOT NULL DEFAULT '';  -- Graph message id
ALTER TABLE mailboxes ADD COLUMN sync_state TEXT NOT NULL DEFAULT ''; -- Graph deltaLink
CREATE INDEX IF NOT EXISTS messages_by_remote ON messages(account_id, remote_id);
```

- Graph 邮件 `uid` 按「文件夹内自增计数（max+1）」填充，保持
  `UNIQUE(account_id, mailbox_id, uid)` 语义；真实身份去重用
  `remote_id`（新 db 助手按 `(account_id, mailbox_id, remote_id)` upsert）。
- Graph 账户的 `imap_host` 等字段填 `graph.microsoft.com` 常量占位（满足
  NOT NULL，UI 不展示）。
- refresh_token 复用 `password_ref` 通道进 `SecretStore`（keyring /
  AES-GCM 本地加密，路径与现有一致）；access_token 仅驻内存。

## 4. 文件清单

| 文件 | 动作 | 职责 |
| --- | --- | --- |
| `backend/src/oauth.rs` | 新建 | PKCE、authorize URL、token 交换/刷新、回环监听、pending 状态 |
| `backend/src/graph.rs` | 新建 | Graph REST 客户端：folders / delta / raw / flags / move / send、role 映射 |
| `backend/src/tokens.rs` | 不建 | （并入 oauth.rs，避免碎文件） |
| `backend/src/model.rs` | 修改 | `Account` 增 4 字段；`ProviderPreset` 增 `provider` |
| `backend/src/db.rs` | 修改 | 迁移 v2、账户读写带新列、`message_upsert_remote`、`mailbox_sync_state` 读写 |
| `backend/src/accounts.rs` | 修改 | 预设、add/update/remove/test 分流、oauth 三方法实现 |
| `backend/src/sync.rs` | 修改 | `sync_account` 按 provider 分流；Graph 文件夹/delta/旗标同步 |
| `backend/src/compose.rs` | 修改 | 投递分流：graph 走 Graph send |
| `backend/src/main.rs` | 修改 | `App` 增 token 缓存与 oauth pending 表；新增 3 个 RPC 分发 |
| `src/lib/rpc.ts` | 修改 | 类型与调用封装 |
| `src/components/AccountWizard.tsx` | 修改 | Graph 表单 + 授权轮询流程 |
| `src/components/SettingsDialog.tsx` | 修改 | 授权状态 + 重新授权 |
| `src/lib/i18n.ts` | 修改 | zh-CN / en 新键 |
| `scripts/mock-graph.mjs` | 新建 | 本地 mock token + Graph 服务器（冒烟用） |
| `scripts/sidecar-check.mjs` | 修改 | 增加 graph 全链路用例 |
| `README.md` | 修改 | 功能表、Azure 注册指引 |
| `docs/版本部署变更说明-v0.2.0.md` | 新建 | 发版说明 |

## 5. 任务分解

> 每任务：实现 → 最小验证（对应单测/typecheck）→ commit。分支
> `feat/m365-outlook-graph`（从 `main` 切出）。测试端点覆盖仅
> `cfg(debug_assertions)` 或 `cfg(test)` 生效（T7 说明），release 构建永远打
> 微软固定端点。

### T1 · Schema v2 + 模型 + db 助手

- `db.rs::migrate`：按 `user_version` 条件补 ALTER（现有结构是
  `CREATE IF NOT EXISTS` + 一次性 user_version，需要加版本分支）。
- `model.rs`：`Account` 增 `provider / auth_type / tenant_id / client_id`
  （serde camelCase，前端类型同步在 T8）。
- `db.rs`：`account_get/add/update/list` 读写新列；新增
  `message_upsert_remote`（按 remote_id upsert，uid 取 `MAX(uid)+1`）、
  `message_remote_id`、`mailbox_get_sync_state` / `mailbox_set_sync_state`。
- 验证：`cargo test`（迁移幂等、upsert 幂等）；手工 `sqlite3` 打开旧库升级。

### T2 · oauth.rs（授权 + token）

- PKCE：`code_verifier`（64 字节随机 → base64url 无填充，43~128 字符）、
  `code_challenge = BASE64URL(SHA256(verifier))`（`sha2` + `base64` 现成）。
- `authorize_url(tenant, client_id, redirect_uri, verifier, state)`：
  端点 `https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize`，
  scope `offline_access https://graph.microsoft.com/Mail.ReadWrite
  https://graph.microsoft.com/Mail.Send https://graph.microsoft.com/User.Read`，
  `response_mode=query`、`code_challenge_method=S256`。
- 回环监听：tokio `TcpListener::bind(("127.0.0.1", 0))`；仅接受
  `GET /?code=&state=`（`urlencoding` + 手写 query 解析，或复用 reqwest 不
  做 server——用 `tokio` 裸 HTTP 读取，参照现有 `stream.rs` 风格）；
  命中后返回 200 HTML「授权完成，请返回 DBX 邮件」，5 分钟超时销毁；
  `state` 不匹配一律 400。
- token 交换/刷新：`POST .../oauth2/v2.0/token`
  （`application/x-www-form-urlencoded`）；解析 `{access_token, refresh_token,
  expires_in}`；**refresh rotation：每次刷新后的新 refresh_token 立即回写
  SecretStore**（旧 token 可能被吊销）。
- `App` 增加：
  - `tokens: Mutex<HashMap<i64, CachedToken>>`（`{access_token, expires_at}`，
    剩余 <120s 触发刷新）；
  - `oauth_pending: Mutex<HashMap<String, PendingAuth>>`（state → verifier、
    tenant、client_id、redirect_uri、result、started_at）。
- 端点 base 做成内部常量 + 测试覆盖点（见 T7）。
- 验证：`cargo test`——PKCE 向量（RFC 7636 附录 B）、URL 编码、token 响应
  解析、rotation 回写（内存 SecretStore 假体）、监听器集成测试
  （`tokio::test` 起 listener + `TcpStream` 模拟回调，断言 state 校验/超时）。

### T3 · graph.rs（REST 客户端）

- `GraphClient::new(account, access_token)`；统一 `GET/POST/PATCH/DELETE` +
  `Authorization: Bearer`；**429/503 按 `Retry-After` 退避重试 ≤3 次**；
  错误解析 `{"error":{"message"}}` 为用户可读文案。
- 文件夹：`GET /v1.0/me/mailFolders?$top=99` + 递归
  `childFolders`（深度 ≤3，跳过 `isHidden`）；`wellKnownFolderName` → 现有
  role 映射（`inbox`→inbox、`sentitems`→sent、`drafts`→drafts、
  `deleteditems`→trash、`junkemail`→junk、`archive`→archive，其余 custom）。
- 增量：每文件夹 `GET /v1.0/me/mailFolders/{id}/messages/delta?$top=50`
  逐 nextLink 翻页；对每个 entry：`@removed` → 按 remote_id 删本地行；
  否则 `GET /v1.0/me/messages/{id}/$value` 取完整 MIME → 写 raw 文件 →
  `mime.rs` 解析 header → `message_upsert_remote`；`isRead / flag.flagStatus /
  hasAttachments` 以列表响应为准覆盖 MIME 解析值；页面尾保存 deltaLink 到
  `mailboxes.sync_state`；404/`SyncStateInvalid` 时清空 deltaLink 全量重建
  （受 `initial_limit` 与同步范围约束）。
- 旗标/移动/删除：`PATCH {id}`（`isRead`、`flag.flagStatus`）、
  `POST {id}/move`、`DELETE {id}`。
- 发送：`send_mime(mime: &[u8])` —— `POST /v1.0/me/messages`
  （`Content-Type: text/plain; charset=UTF-8`，body=MIME）建草稿（保留
  Message-ID/References 头）→ `POST /v1.0/me/messages/{id}/send`；若 MIME
  建草稿因附件大小被拒（以官方文档/实测阈值），回退：JSON 建草稿 +
  `POST /v1.0/me/messages/{id}/attachments/createUploadSession` 分片上传 +
  send。沿用 25MB 单附件 / 40MB 总量上限。
- `answered` 旗标：Graph 列表无对应项，按 `!in_reply_to.is_empty()` 近似
  （文档注明差异）。
- 验证：`cargo test`——role 映射、delta 页解析（含 `@removed`）、错误体解析、
  上传会话分片切分（纯函数）。

### T4 · 账户层接线

- `presets()`：新增
  `ProviderPreset { key:"msgraph", label:"Microsoft 365 / Outlook.com (Graph)",
  provider:"graph", domains:[outlook.com/hotmail.com/live.com/microsoft.com/office365.com], hint:"需在 Azure Portal 注册应用并填入 Client ID，之后浏览器授权" }`；
  `outlook` 预设 hint 改为「微软已停用基本认证，建议改用 Microsoft 365
  (Graph) 预设」。
- `accounts::add`：graph 分支——校验 email/clientId/tenantId 非空 →
  `oauth_pending` 按 `oauthState` 认领 token 成功 → refresh_token 存
  SecretStore（`password_ref = oauth2:{email}`）→ 落库（imap/smtp 字段填
  Graph 占位常量）→ 立即首同步（复用 `sync/now` 的调度路径）。
- `accounts::update`：graph 账户允许改 tenant/clientId（不改则跳过）、
  不触发密码分支；「重新授权」= 前端重走 oauthBegin → update 携带新
  oauthState。
- `accounts::test`：graph 账户 → 刷 token + `GET /v1.0/me`。
- `main.rs` 分发：`account/oauthBegin`（起 listener、生成 state、返回
  authorizeUrl）、`account/oauthStatus`（轮询）、`account/oauthCancel`；
  pending 10 分钟过期清理。
- 验证：`cargo test`（add graph 分支用假 pending token + 内存 SecretStore；
  重复 email 拒绝沿用现有逻辑）。

### T5 · 同步分流

- `sync_account` 开头按 `account.provider` 分流：graph 账户走
  `graph::sync_account`（不建 IMAP 连接、不读密码读 refresh_token）。
- Graph 同步流程：folders 列表 → `mailbox_upsert`（Graph 用 folder id 当
  `name`？——否：`mailboxes.name` 存 displayName 保持 UI 一致，新增列
  不必要，Graph folder id 存 `sync_state` 之外的
  `remote_id`?）
  ——**定案：`mailboxes.name` 存 displayName；Graph folder id 需要持久化**，
  加列 `mailboxes.remote_id TEXT NOT NULL DEFAULT ''`（归入 T1 迁移 v2），
  delta/flags/move 全按 remote_id 寻址；同一迁移一并完成。
- 旗标双向：`pending_flags` 复用——graph 账户的待推旗标经
  `PATCH` 提交；远端变更在 delta 里带回（`isRead/flag` 覆盖）。
- `ensure_body`：graph 账户 raw 已在同步期落盘，正文缺失时按 remote_id
  `GET $value` 补拉。
- 巡检/手动同步/`sync_enabled`/`sync_interval`/同步范围设置沿用现有
  `sync_account` 外层逻辑（只换传输）。
- 验证：`cargo test` + T7 冒烟覆盖本任务。

### T6 · 发送分流

- `compose.rs::deliver`：构建 `Outgoing` → `smtp::build()` 不变；
  `account.provider == "graph"` 时走 `graph::send_mime`（在
  `app.rt.block_on` 内，与现有 SMTP 路径同构）；成功后本地 Sent 副本策略：
  Graph 自动落 Sent Items，delta 同步会拉回，**不再本地 append**（与 IMAP
  行为对齐的判定在实现时核对：IMAP 路径现在是 SMTP 成功后 append Sent，
  graph 路径跳过 append）。
- 失败处理沿用 outbox 重试机制；401 时刷新 token 重试一次。
- 验证：`cargo test`（deliver 分支用假 token 服务）+ T7。

### T7 · Mock 端到端冒烟

- `scripts/mock-graph.mjs`（Node，无第三方依赖，参照现有脚本风格）：
  - `POST {token}/oauth2/v2.0/token`：校验 `code_verifier` 对
    `code_challenge`（S256）、一次性 code、刷新轮换 refresh_token；
  - `GET {authorize}/oauth2/v2.0/authorize`：发一次性 code（测试脚本充当
    「用户代理」解析 redirect_uri 端口后回调 `http://localhost:{port}/`）；
  - `GET /v1.0/me`、`GET /v1.0/me/mailFolders`（inbox/sent/drafts/deleted/
    junk + 自定义，含 childFolders 一层）、
    `GET /v1.0/me/mailFolders/{id}/messages/delta`（fixtures：每文件夹
    若干完整 RFC822 原文 + 增量场景）、`GET /v1.0/me/messages/{id}/$value`、
    `PATCH {id}`、`POST {id}/move`、`DELETE {id}`、
    `POST /v1.0/me/messages`（MIME 建草稿，校验头保留）、`POST {id}/send`。
- 端点覆盖机制：sidecar 在**仅 `cfg(debug_assertions)` 且
  `DBX_MAIL_TEST_ENDPOINTS=1`** 时读取 `DBX_MAIL_TOKEN_BASE` /
  `DBX_MAIL_GRAPH_BASE` / `DBX_MAIL_AUTHORIZE_BASE`；release 构建不编译该
  分支行为。
- `sidecar-check.mjs` 增 graph 用例：bootstrap → oauthBegin → 模拟回调 →
  oauthStatus=done → account/add(graph) → sync/now（断言 folders/messages/
  正文）→ mail/flags → mail/move → compose/send（断言 mock 收到的 MIME 保留
  Message-ID）→ 增量 delta（改 fixture → 再 sync → 断言新邮件与删除生效）→
  token 轮换（refresh 后 mock 拒绝旧 token，断言 sidecar 用新 token）。
- 验证：`node scripts/sidecar-check.mjs` 全绿。

### T8 · 前端

- `rpc.ts`：`ProviderPreset.provider?: "imap" | "graph"`；`Account` 增
  `provider / tenantId / clientId`（camelCase 与 Rust serde 对齐）；新增
  `oauthBegin / oauthStatus / oauthCancel` 封装（Status 轮询 2s 间隔、
  上限 5 分钟）。
- `AccountWizard.tsx`：选中 graph 预设（或域名匹配）→ 展示 email、显示名、
  tenant（默认 `common`）、client_id 输入 + Azure 注册指引（5 步精简文案 +
  文档链接）；「开始授权」→ `oauthBegin` → `window.open(authorizeUrl)`
  （失败则展示可复制链接）→ 轮询 status → done 后 `account/add` 携带
  oauthState → 进入首同步。
- `SettingsDialog.tsx`：graph 账户显示「OAuth2 已授权 · {tenantId}」+
  「重新授权」按钮（重走授权流后 `account/update`）；密码栏隐藏。
- `i18n.ts`：zh-CN / en 全部新键（向导、授权页提示、失败文案、重新授权）。
- 验证：`tsc --noEmit`（或 package.json 里的 typecheck script）+
  `node scripts/render-check*.mjs` + `sanitize-check.mjs`。

### T9 · 文档收尾

- `README.md`：功能表账户行加「Microsoft 365 / Outlook.com（Graph API，
  OAuth2 授权码 + PKCE）」；新增「Azure 应用注册」小节（第 1 节内容）。
- 协议变更写入 README 协议段（本仓库协议说明所在位置）。
- `docs/版本部署变更说明-v0.2.0.md`：功能、升级注意（Azure 自备应用）、
  已知差异（answered 近似、Sent 副本策略）。
- 真租户手动验收清单（交维护者执行）：个人 outlook.com + 组织 M365 各一、
  受限租户管理员同意提示、大附件发送（>4MB 回退路径）、delta 断链重建、
  五平台打包（CI 自动）。

## 6. 验收标准（完成定义）

1. `cargo test`（backend）全绿；`cargo build --release` 通过。
2. `node scripts/sidecar-check.mjs` 含 graph 全链路用例全绿。
3. 前端 typecheck + render/sanitize checks 全绿。
4. 协议文档、README、变更说明 v0.2.0 齐。
5. 七语规则不适用（本仓库 i18n 为 zh-CN/en，与现状对齐）。
6. 真租户验收清单输出（本机无 M365 环境，如实标注为未执行项）。

## 7. 风险与开放问题

- **MIME 建草稿附件上限**：官方文档对 MIME 方式的附件限制表述不一（≈4MB
  量级）；实现时以文档 + mock/真租户实测为准，回退路径
  （JSON + createUploadSession）已备。
- **受限租户**：`User.Read` 等委托权限一般无需管理员同意；租户策略可强制
  全部同意，属用户环境问题，文档注明。
- **回环端口被占/防火墙**：`bind :0` 随机端口风险极低；macOS 防火墙对
  localhost 监听不拦截，Windows 首次可能弹允许提示（文档注明）。
- **refresh_token 轮换竞态**：单进程内 `tokens` 锁串行刷新即可；外部多实例
  （DBX 多开同账户）不在范围内。
- **性能**：delta 每条消息额外一次 `$value` GET，与 IMAP 同步成本相当；
  大文件夹首拉受 `initial_limit`（≤2000）约束。
