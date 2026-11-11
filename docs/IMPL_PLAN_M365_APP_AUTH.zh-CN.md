# 实施计划：M365 双模接入（纯后台 client_credentials + 机密客户端授权码）

> 分支：`feat/m365-outlook-graph`。目标：在既有浏览器授权（公共客户端 PKCE）
> 之外，支持两种新形态——
> **纯后台接入**（`client_credentials` 应用令牌，无浏览器交互，直访指定邮箱）
> 与**机密客户端授权码**（token 交换/刷新携带 `clientSecret`）。
> 触发背景：真实验收只给了 clientId + clientSecret + tenantId + userId 四件套，
> 且租户应用已配置应用程序权限并完成管理员同意。

## 0. 关键决策

- **auth_type 扩展**：`oauth2`（浏览器授权，不变）/ `oauth2_app`（纯后台）。
  判定：provider=graph 且无 `oauthState` 且 `clientSecret`、`userId` 齐备。
  机密客户端授权码不引入新 auth_type——`oauth2` + `client_secret_ref` 非空即携带 secret。
- **Secret 语义**：`clientSecret` 只进 SecretStore（独立通道 `graphsecret:<email>`），
  SQLite 只存引用列 `client_secret_ref`（v4），绝不落明文、不回传前端。
- **Graph 路径参数化**：`GraphClient` 的 base 含路径段
  `{graph_base}/v1.0/me` 或 `.../users/{user_id}`；应用令牌无 /me 语义。
- **令牌缓存复用**：`oauth2_app` 走 `client_credentials_token`（无 refresh_token，
  过期即重取），缓存与授权码令牌共用 `TokenCache`。
- **宿主连接（connection-provider）**：初版决策是「不受影响，纯后台只走插件内
  向导」（当时认为表单 secret 语义与「凭据不落盘」冲突）。**2026-11-09 更新：
  已推翻**——宿主表单 `binding:"secret"` 与 IMAP 密码走同一条「宿主持有 →
  生命周期下发 → 内存会话」路径，Graph `auth_mode=app` 纯后台接入现已在
  宿主连接管理器全量支持（见 IMPL_PLAN_CONNECTION_PROVIDER.zh-CN.md §1.1）。
- **debug 钥匙串开关**：`DBX_MAIL_SECRET_FILE=1` 强制本地加密通道——无人值守跑
  真实验收时，新二进制首次读取 macOS 钥匙串会触发授权弹窗并挂起。

## 1. 变更面

| 文件 | 变更 |
| --- | --- |
| `db.rs` | v4：accounts 增 `user_id`、`client_secret_ref`；INSERT/UPDATE/apply_host 带新列；`account_(set_)client_secret_ref` |
| `model.rs` | `Account.user_id`（camelCase 序列化为 `userId`，前端可见） |
| `oauth.rs` | `exchange_code`/`refresh_token` 可选 secret；`begin` 带 secret（驻内存 PendingAuth）；`client_credentials_token` + `app_token_set`；`access_token` 按 auth_type 分流；`load_client_secret` |
| `graph.rs` | `GraphClient::new(token, user_id)`；10 处 `/v1.0/me` 字面量归入 base 路径段 |
| `sync.rs` | 8 处 GraphClient 构造传 `account.user_id`（access_token 内部自动选 app 令牌） |
| `accounts.rs` | add：纯后台分支（校验 userId/secret、verify=换令牌+探测 folders、secret 落库+引用落库）；机密客户端 secret 引用落库；update：支持更新 clientSecret；`account_from_params` 设 `user_id`/`auth_type` |
| `main.rs` | `account/oauthBegin` 透传 clientSecret |
| `secrets.rs` | debug 专用 `DBX_MAIL_SECRET_FILE=1` 强制本地加密通道 |
| 前端 `AccountWizard.tsx` | Graph 面板「接入方式」双模式；app 模式必填 secret/userId；browser 可选 secret |
| 前端 `rpc.ts` | `oauthBegin(tenantId, clientId, clientSecret?)` |
| 前端 `i18n.ts` | `wizard.graphMode*` / `clientSecret*` / `userId*` / `appGuide`（zh-CN + en） |
| `scripts/mock-graph.mjs` | token 端点支持 `client_credentials`（校验 `appSecret`）；`/v1.0/users/{id}/...` 归一化到 `/me` 路由 |
| `scripts/sidecar-check.mjs` | 「M365 纯后台接入」冒烟节：缺 userId 报错 / 错 secret 验证失败 / 正常创建 + 发现 + 读 mock 邮件 |
| `scripts/real-m365-check.mjs` | 真实租户连通性：OIDC/authorize/token + 应用令牌 + 只读邮箱探测 |
| `scripts/real-m365-app-check.mjs` | 真实端到端（只读）：sidecar + account/add 纯后台 → 发现 → 同步 → 读信 |

## 2. 验收

- [x] `cargo test` 46/46 全绿（含 graph spec、连接生命周期、MCP、迁移 v4）
- [x] `sidecar-check` 全绿（含新增纯后台节；Graph 全链路 debug 构建）
- [x] `npm run typecheck` 通过（前端 zh-CN/en 文案齐全）
- [x] 真实租户连通性：`M365_CLIENT_SECRET=… node scripts/real-m365-check.mjs …` 全绿
      （OIDC、登录页、token 端点、应用令牌、`/users/{id}/mailFolders` 只读探测、
      收件箱最近 3 封只读探测）
- [x] 真实端到端：握手 / oauth2_app 账户创建 / 真实文件夹发现 / 本地文件夹就位 /
      限量首拉同步无错误 / **读到真实邮件** / **按需取到真实正文**——7/7 全绿
      （`kn.warehouse@kuehne-nagel.com`，只读边界）
- [ ] 写操作真实验收（发送/移动/旗标）——需用户明确授权后执行，默认只读

## 3. 收尾与真实验证记录（2026-11-06）

- 真实验证用四件套（tenantId/clientId/clientSecret/userId，凭据仅经环境变量注入）：
  - `real-m365-check.mjs`：OIDC、授权登录页、token 端点活性、应用令牌、
    `/users/{id}/mailFolders` 与收件箱最近 3 封只读探测——**全绿**（应用程序权限
    已管理员同意）。
  - `real-m365-app-check.mjs`（插件本体端到端，只读）：握手 / oauth2_app 账户创建 /
    真实文件夹发现 / 本地文件夹就位——**全绿**；首拉与读信见下。
- 排查记录（前几轮超时均非死锁）：
  1. 新编译二进制首次读 macOS 钥匙串触发授权弹窗挂起 → 加 `DBX_MAIL_SECRET_FILE=1`
     开关（debug 专用）。
  2. 真实邮箱首拉耗时：Inbox 289 + Sent 720 封、每封完整 MIME（Graph 账户设计
     语义），全量约 7-20 分钟；验收脚本以 `limit:50` 限量。
  3. 协议 `mailbox/list` 是**全账户混合列表**（跨账户设计）：验收脚本误把它当
     单账户列表取 inbox 行，拿错账户导致读信为空——已按 `accountId` 过滤。
  4. 周期巡检线程可能正占同步槽做全量首拉，`sync/now` 立即返回「正在同步中」——
     验收脚本改为等待重试。
  5. 顺手修复既有真 bug：连接管理线 `ensure_account_row` 在 `or_else` 闭包里
     二次加 db 锁的死锁（首次宿主连接必现）。
  6. 网络环境：本机对 `graph.microsoft.com` 的 TLS 直连间歇被阻断
     （`SSL_ERROR_SYSCALL`，login 域不受影响）。reqwest 默认只读
     `HTTPS_PROXY` 环境变量、不读 macOS 系统代理——验收脚本以
     `HTTPS_PROXY=http://127.0.0.1:<本机代理端口>` 注入即可稳定复跑。
     生产用户侧同理：网络受限环境需系统级代理（TUN/增强模式）。
- 写操作（发送/移动/旗标）真实验收默认不执行（只读边界）；mock 冒烟已覆盖
  发送/旗标/移动全链路（dryRun 两阶段、撤销窗口、已发送回流）。

## 4. 非目标

- 不实现 ROPC（密码换令牌，微软已弃用且与 MFA/条件访问冲突）。
- 不在宿主连接表单里做纯后台接入（见决策）；Graph 连接仍走插件内授权。
  （2026-11-09 更新：决策已推翻，宿主表单现支持 `auth_mode=app` 纯后台。）
- 不改 IMAP/SMTP 账户与 MCP 工具面（工具经 `access_token` 自动适配两种令牌）。
