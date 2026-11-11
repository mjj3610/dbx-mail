# 实施计划：接入 DBX 连接管理（connection-provider）

> 分支：`feat/m365-outlook-graph`。目标：用户可以在 **DBX 宿主连接管理器**里创建、
> 测试、连接、断开邮件账户，复用插件既有的 IMAP/SMTP 探测与同步引擎；
> 插件内向导继续保留（Graph/OAuth 账户唯一入口）。
>
> 核心原则：**宿主连接是凭据的持有者，插件只在生命周期请求里短暂见到 Secret，
> 一律不落盘**；`account/*` 现有方法与行为零改动。

## 0. 背景与已核实的仓库事实

- DBX 通过 `connection-provider` 贡献点驱动插件连接：宿主渲染并保存表单，
  固定调用后端 `connection/test`、`connection/connect`、`connection/disconnect`
  （自定义动作走 `connection/action`，本线不涉及）。
- 生命周期请求形状（host `crates/dbx-plugin-runtime/src/plugins/host.rs::plugin_connection_params`）：

  ```json
  {
    "provider":   { "id": "com.mjj3610.mail.connection", "databaseType": "mail" },
    "connection": { "id": "…", "name": "…", "db_type": "plugin",
                    "host": "…", "port": 993, "username": "…", "password": "",
                    "external_config":    { "<field.key>": "…" },
                    "connection_secrets": { "<field.key>": "…" },
                    "plugin_connection_type": "mail" },
    "runtime":    { "host": "127.0.0.1", "port": 49152 },
    "operationId": "…"
  }
  ```

  - `binding: "name"/"host"/"port"/"username"/"password"` 落到 `connection` 的
    标准字段；`binding: "config"` 落 `external_config[field.key]`；
    `binding: "secret"` 落 `connection_secrets[field.key]`（不进 `config_json`）。
  - `runtime.host` / `runtime.port` 是经宿主代理后的**最终逻辑端点**，协议要求直接连它。
  - `connection/disconnect` 可能只带 `{ "connection": { "id": "…" } }`，解析各段均可缺省。
  - fields 合法类型（host 侧 manifest.schema.json `formField.type` 枚举）：
    `text / password / number / boolean / select / radio / textarea`；
    `binding: "port"` 必须 `number`；`secret/name/host/username/password/database`
    binding 只能配文本类类型；`select` 必须带非空 `options`。
- 本插件现状（`backend/src/`）：
  - 账户模型 `model.rs::Account`（camelCase 序列化给前端），本地 SQLite
    `accounts` 表（`email UNIQUE`，`password_ref` 指向系统凭据库/本地加密文件）。
  - `accounts::test` → `sync::test_account`：IMAP 登录 + LIST + INBOX STATUS，
    再 SMTP 探测；`SmtpLogin` 复用**同一个** username/password（后端本就只有一套凭据）。
  - `accounts::discover`：登录并灌入文件夹；`sync::sync_account`：增量同步引擎，
    密码经 `App::load_password(account_id)` 从 SecretStore 取。
  - 周期巡检线程按 `account.sync_enabled` 过滤，不启用的账户不会报错刷屏。
  - `db.rs` 迁移机制：`user_version` 递增 + `ALTER TABLE`（当前 v2）。

## 1. 关键决策与理由

### 1.1 Graph 双模式：browser 建授权壳；app 纯后台接入（2026-11-09 更新）

> 初版决策是「Provider 只覆盖 IMAP/SMTP；Graph 仍走插件内向导」。M365 纯后台
> 接入落地后，宿主表单已支持 Graph 双模式，本节按现状改写。

- Graph `browser`：宿主连接表单只建账户壳（`auth_type=oauth2`），浏览器交互
  授权仍在插件内向导 /「重新授权」里完成；未授权前不进周期巡检。
- Graph `app`（`auth_mode=app`）：纯后台接入（client_credentials）。`client_secret`
  走 `binding:"secret"` → `connection_secrets` → 内存会话表（与 IMAP 密码同一
  「凭据不落盘」路径，初版担心的 secret 语义冲突不成立）；`user_id` 缺省回退
  邮箱地址；`auth_type=oauth2_app` 且 `sync_enabled=true`，connect 时换令牌
  验证成功后即后台首拉。
- Graph 账户无 host/port 语义这一点不变；IMAP/SMTP 生命周期行为零改动。

### 1.2 fields 与后端账户参数对齐（snake_case），SMTP 复用同一套凭据

后端账户模型只有一套 username/password（`accounts::account_from_params` 亦如此）。
**注意：field.key 必须满足宿主 `valid_identifier` 规范（仅小写字母/数字与 `.``-``_`，
不允许大写）**，故 fields 为：`display_name、email、host、port、security、username、
password、smtp_host、smtp_port、smtp_security`。**不设** `smtpUsername/smtpPassword`
（后端无处安放，避免假字段）。sidecar 在 `connection/*` 解析处把 external_config 键
映射到 `account/*` 的 camelCase 参数。

| field.key | type | binding | 存储 | 说明 |
| --- | --- | --- | --- | --- |
| `display_name` | text | `name` | connection.name | 连接显示名，缺省回退邮箱 local-part |
| `email` | text | `config` | external_config.email | **必填**，本地账户行的唯一标识（`accounts.email UNIQUE`） |
| `host` | text | `host` | connection.host | **必填**，IMAP 服务器（表单语义）；实际拨号用 runtime 端点 |
| `port` | number | `port` | connection.port | 默认 993 |
| `security` | select | `config` | external_config.security | `tls`（默认）/`starttls`/`none` |
| `username` | text | `username` | connection.username | **必填**，IMAP/SMTP 登录名 |
| `password` | password | `secret` | connection_secrets.password | **必填**；省略 binding 的 password 型字段默认也按 secret 处理，显式声明 |
| `smtp_host` | text | `config` | external_config.smtp_host | **必填**（无预设可兜底） |
| `smtp_port` | number | `config` | external_config.smtp_port | 默认 465 |
| `smtp_security` | select | `config` | external_config.smtp_security | `tls`（默认）/`starttls`/`none` |

`capabilities: ["test", "connect", "disconnect"]`；`workbench` 指向既有工作台；
`database_type: "mail"`；不声明 `proxy_route`（邮件只有单一 IMAP 端点，SMTP 由
sidecar 直接出网，见 1.4）。

### 1.3 会话模型：按 `connection.id` 键控的内存会话 + 账户行复用（最小侵入）

两个候选：
- **A. host 连接映射为独立的内部账户行**（全新一行、不复用向导账户）：
  遇到 `accounts.email UNIQUE` 冲突（同一邮箱既在向导建过、又从宿主连接）需要
  变通，且两行缓存割裂。
- **B.（采纳）内存会话表 + 复用同邮箱账户行**：
  - `App` 新增 `host_sessions: Mutex<HashMap<String /*connection.id*/, HostSession>>`，
    `HostSession { connection_id, account_id, password }`——**密码只在内存**，
    生命周期请求之外任何路径都拿不到。
  - `App::load_password` 先查会话内存（宿主连接的密码），未命中再走既有
    SecretStore 路径——`sync.rs`/`accounts.rs` 的取密逻辑零改动。
  - 账户行落点：按 `host_connection_id` → 按同 `email` → 新建，三级复用同一行；
    连接期把行打上 `host_bound = 1` + `host_connection_id`，刷新服务器端点等
    连接相关列；断开时 `host_bound` 复位（向导建的行恢复向导语义）。
  - host 连接**新建**的行 `sync_enabled = 0`：周期巡检只在会话存活时才该跑，
    而巡检线程不感知会话表；手动「立即同步」（`sync/now`）在会话存活期间照常可用。
- 理由：改动集中在一个新模块 + `App` 一个字段 + `load_password` 三行 +
  db v3 两列；同步引擎、IMAP/SMTP 传输层、前端全部复用。

### 1.4 端点策略：IMAP 连 `runtime.*`，SMTP 直连

- `runtime.host:runtime.port` 是宿主代理后的 IMAP 最终逻辑端点，`connection/test`、
  `connection/connect`、后续同步一律以它为准，并把它**持久化为账户行的
  `imap_host/imap_port`**（同步引擎只会读库）。代价：宿主隧道关闭后手动同步会
  连接失败——这是代理端点的固有语义，重连即恢复。
- SMTP：宿主只代理一个端点，`smtp_host:smtp_port` 由 sidecar 直接出网
  （sidecar 本就承担全部网络职责，与插件内向导账户行为一致）。

### 1.5 生命周期语义

| 方法 | 行为 | 返回 |
| --- | --- | --- |
| `connection/test` | 构造 `Account`（IMAP 端点取 runtime）后复用 `sync::test_account` 探测 IMAP+SMTP | 成功 `{success:true, message:"IMAP 登录成功…；SMTP 连接成功…"}`；失败 `{success:false, message:"连接失败：…"}`（**不抛 RPC error**，按契约以 success 表达） |
| `connection/connect` | ① 解析参数；② 幂等检查（同 `connection.id` 已有活跃会话且账户行在 → 直接成功返回）；③ 确保账户行（三级复用）；④ 先把会话写入内存表，再复用 `accounts::discover` 拉文件夹；⑤ 失败则回滚会话并返回错误；⑥ 后台线程首拉近 100 封，**不阻塞 connect 返回** | `{success:true, message:"已连接 <email>"}` |
| `connection/disconnect` | 从会话表移除（密码随之丢弃）、`host_bound` 复位；账户行与全部本地缓存保留；未知 id 亦成功（幂等） | `{success:true}` |

## 2. 协议变更（后端新增，`account/*` 不动）

新增三个分发分支（`backend/src/main.rs::dispatch`，置于 `account/*` 之后）：
`connection/test`、`connection/connect`、`connection/disconnect`，
全部由新模块 `backend/src/hostconn.rs` 实现。

## 3. 存储变更（db.rs v3）

```sql
ALTER TABLE accounts ADD COLUMN host_bound          INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN host_connection_id  TEXT    NOT NULL DEFAULT '';
```

- `model.rs::Account` 增加对应 `hostBound: bool`、`hostConnectionId: String`
  （`#[serde(default)]`，前端 TS 类型不感知多出的字段）。
- 新增 db 函数（不动既有 `account_insert/account_update/account_*` 的行为）：
  `account_find_by_host`、`account_find_by_email`、`account_apply_host`
  （定点 UPDATE：身份列 + 服务器端点列 + host 标记列）、`account_unbind_host`。
- `accounts.rs::account_from_params` 的构造处补两个新字段默认值（不改变行为）。

## 4. hostconn.rs 模块结构

```
HostSession { connection_id, account_id, password }   // 密码仅内存
spec_from_lifecycle(params) -> Spec { connection_id, account, password }   // 纯函数，可单测
session_password(app, account_id) -> Option<String>   // 供 App::load_password 优先查
session_active(app, connection_id) -> bool            // connect 幂等判断
test(app, params) / connect(app, params) / disconnect(app, params)
```

安全红线：Secret 不进日志/事件/错误消息；错误文案只含主机与阶段，不含密码。

## 5. sidecar-check.mjs 用例（沿用现有风格）

先探测 `connection/disconnect`（未知 id）：若返回「未实现」错误则整节 SKIP
（旧二进制不 FAIL）。已实现时覆盖：
1. `connection/test` 失败路径：指向 `127.0.0.1:1`（必然拒绝）→ `success:false`
   且 message 非空；
2. `connection/connect` 幂等（失败路径一致性）：同一 `connection.id` 连不可达
   服务器两次，两次结果一致且不崩溃、不留活跃会话；
3. `connection/disconnect` 幂等：未知 id 断开两次均 `{success:true}`。
   成功路径的幂等（真实会话建立/复用）由 Rust 单测覆盖内存表语义。

## 6. 单测（hostconn.rs `#[cfg(test)]`）

1. `spec_from_lifecycle` 映射：runtime 端点覆盖、external_config/secret 取值、
   security 归一化、smtp 缺省（端口 465、security tls）、缺 `connection.id`/
   `email`/`smtp_host`/`password` 报错。
2. test 失败路径：自建 tokio runtime 对 `127.0.0.1:1` 探测返回 Err（不 panic、
   不含密码）。
3. connect 幂等语义：内存会话表的插入/覆盖/移除/按账户查密码。

## 7. 文档与 UI

- `README.md` 增「DBX 连接管理」小节：宿主里怎么建连接、字段表、与插件内向导
  的关系（Graph 走向导；同一邮箱两边建会复用同一份本地缓存）。
- UI 来源标记：**本期不做**（涉及 Sidebar/Settings 与双语文案，改动面大，
  留待宿主联调后单独一线）。

## 8. 任务清单

- [x] docs/IMPL_PLAN_CONNECTION_PROVIDER.zh-CN.md（本文档）
- [x] manifest.json：`connection-provider` 贡献点 + zh-CN/en 本地化
- [x] db.rs：v3 迁移 + 4 个 host 相关 db 函数
- [x] model.rs / accounts.rs：Account 新字段与默认值
- [x] main.rs：App 加 `host_sessions`、`load_password` 会话优先、三个分发分支
- [x] hostconn.rs：解析/映射、test/connect/disconnect、幂等会话表
- [x] hostconn.rs 单测（映射、失败路径、幂等）
- [x] scripts/sidecar-check.mjs：connection/* 用例（含 SKIP 探测）
- [x] README.md：「DBX 连接管理」小节
- [ ] 验证：`cargo test` 全绿；`node scripts/sidecar-check.mjs` 全绿

## 9. 遗留风险

- 宿主隧道断开后，host 账户的手动同步会失败（端点已失效），重连恢复；
  UI 侧错误展示沿用 `last_error` 通道。
- 同一邮箱从宿主连接与向导各建一次时共享同一账户行，连接期凭据以宿主为准
  （内存覆盖），断开后恢复向导凭据。
- `connection/connect` 的后台首拉与宿主的 connect 超时解耦：宿主只看到
  「会话已建立」，首拉进度暂无事件通道（插件未申请 `host.events`）。
