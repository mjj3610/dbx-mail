# DBX 邮件（dbx-mail）

本地优先的多账户邮件工作台，作为 **DBX 插件**运行。
邮件副本、正文、附件与草稿都缓存在本机 SQLite 与数据目录里，**离线也能读、能写、能排队发送**；
所有文件系统操作（附件落盘、`.eml` 导入导出、目录浏览、原始邮件缓存）都由 Rust Sidecar 完成，
插件页面本身没有文件系统与网络权限。

界面与信息架构参考开源邮件客户端 [Penguin Mail](https://github.com/t8y2/penguin-mail) 1.0.11，
按 DBX 插件体系重新实现（代码为独立编写，未复制其 Rust/GTK 源码）。

## 0. 安装

从 [Releases](https://github.com/mjj3610/dbx-mail/releases) 下载对应平台的
`com.mjj3610.mail-<版本>-<平台>.dbxp`（`windows-x64` / `linux-x64` / `linux-arm64` / `darwin-x64` / `darwin-arm64`，
由 CI 在各自原生 runner 上构建），或在 DBX 的插件商店里搜索「邮件 / DBX Mail」。
当前版本 **0.1.5**（五平台未签名包）。

![三栏工作台](screenshots/01-inbox.png)

## 1. 功能

| 模块 | 能力 |
| --- | --- |
| 账户 | IMAP/SMTP 多账户；8 种服务商预设（Gmail / Outlook / QQ / 腾讯企业邮 / 163 / 阿里云 / iCloud / Yahoo）按域名自动填服务器；手动改 host/port/加密方式；添加前测试连接；文件夹发现；**Microsoft 365 / Outlook.com 走 Graph API**（见下节）；**DBX 连接管理器可直接建邮件账户**（IMAP/SMTP 或 Graph 壳，见 §1.3） |
| AI 工具 | MCP 工具面：`mail_list` / `mail_read` / `mail_flag` / `mail_move` / `mail_send`（dryRun 两阶段）/ `folder_list`，进入内置 AI 与外部 dbx MCP 服务（见 §1.2） |
| 密码 | 系统凭据库（Windows Credential Manager / macOS Keychain / Secret Service）；不可用时退化为 AES-256-GCM 本地加密文件（`secrets.key` + `secrets.bin`，Unix 0600） |
| 同步 | IMAP 增量同步：`UIDVALIDITY` 变化时重置文件夹、按 `last_uid` 拉新头、旗标双向同步（`pending_flags` 表避免服务器覆盖本地改动）；Graph 账户按 `deltaLink` 增量、旗标/移动/删除直推；同步范围可选「主要文件夹 / 全部」；启动 45 秒巡检、可手动同步、可关自动同步 |
| 会话列表 | 按 `References`/`In-Reply-To`/`Message-ID` 归并会话，会话内计数与未读聚合；过滤（全部 / 未读 / 星标 / 附件）；跨账户「所有收件箱」；搜索主题、摘要、发件人、收件人与正文全文；分页加载 |
| 阅读器 | 会话内多封邮件折叠展开；正文由 `mail-parser` 解析后本地渲染；HTML 先经 DOMPurify 白名单清洗；**远程图片默认不加载**——挂载前就把 `src` 换成占位，不会自动向图床发起任何请求；用户点「显示图片」后由本地后端代理取回并转 data URL（SSRF 白名单 + 逐跳重定向复核，拒绝环回、内网与 CGNAT） |
| 附件 | 解析 `mail-parser` 的部件索引，按需从原始邮件取字节；保存时自绘目录选择器选目录、同名自动加序号不覆盖；25 MB 单文件上限（`mail/part` 30 MB） |
| 撰写 | 富文本（加粗 / 斜体 / 下划线 / 删除线 / 等宽 / 链接 / 引用 / 列表 / 清格式）；多收件人、抄送、密送；多账户切换发件人；签名自动插入；附件拖入或选择；`Ctrl/⌘ + Enter` 发送 |
| 草稿 | 每 4.5 秒自动存草稿；关闭时可选「存为草稿 / 放弃」；草稿箱可继续编辑，发送后自动删除 |
| 发件箱 | 立即发送也先入队（留 `undoDelay` 撤销窗口）；定时发送（10 分钟 / 1 小时 / 明早 9:00 / 自定义时间戳）；后台每 2 秒投递，失败累计次数并可重试；崩溃重启后把 `sending` 复位为 `queued` |
| 归档与删除 | 存档 / 垃圾 / 已删除 / 移动到指定文件夹；IMAP 侧优先 `MOVE`，不支持时退化为 `COPY + \Deleted + EXPUNGE` |
| 导入导出 | `.eml` 导出到选定目录（按主题截断命名、同名加序号）；`.eml` 导入到当前文件夹（本地建档、写正文与附件缓存） |
| 界面 | 三栏（侧栏 / 会话列表 / 阅读器）、按会话聚合、深浅色（跟随系统 / 手动）、列表密度、简中 / 英文、窄屏单栏、`j/k` `c` `r` `a` `f` `e` `#` `s` `u` `/` 快捷键 |
| 演示模式 | 地址后加 `?demo=1` 且无 Sidecar 时使用内存数据，用于没有邮箱时预览界面 |

![阅读器](screenshots/02-reader.png)

## 1.1 Microsoft 365 / Outlook.com（Graph API）

微软已对 Microsoft 365 与 Outlook.com 停用 IMAP/SMTP 基本认证，纯密码登录不可用。
此场景走 **Microsoft Graph REST**（与 IMAP 账户共用同一份本地缓存，会话、搜索、
阅读、附件、草稿、发件箱体验一致）。**两种接入方式**（向导里选择）：

- **浏览器授权（推荐）**：OAuth2 授权码 + PKCE，sidecar 在 `127.0.0.1` 随机端口
  接收回调；refresh_token 存系统凭据库（同密码通道），access_token 只驻内存并
  到期自动刷新。
- **纯后台接入（client_credentials）**：无需浏览器交互——Azure 应用配置
  Mail.ReadWrite、Mail.Send、User.Read 的**「应用程序」权限并完成管理员同意**后，
  填 Client Secret + 目标邮箱（userId），插件直接以应用令牌访问该邮箱
  （Graph `/users/{id}/...`）。适合服务器/无人值守场景；应用令牌无 refresh_token，
  过期自动重取，Secret 只存系统凭据库（引用列在本地 SQLite，不含明文）。
  机密客户端也可在浏览器授权模式下填写 Client Secret（token 交换与刷新携带）。
- **自备 Azure 应用**：授权使用你自己在 Azure Portal 注册的应用（公共客户端，无 secret）：

  1. 登录 [portal.azure.com](https://portal.azure.com) → Microsoft Entra ID → 应用注册 → 新注册；
  2. 受支持账户类型选「任何组织目录中的账户和个人 Microsoft 账户」；
  3. 重定向 URI：平台选「移动和桌面应用程序」，填 `http://localhost`（不带端口）；
  4. API 权限 → Microsoft Graph → 委托权限：`offline_access`、`Mail.ReadWrite`、`Mail.Send`、`User.Read`；
     受限租户可能需要管理员同意；
  5. 把「应用程序(客户端) ID」填入插件向导，租户填 `common`（或组织租户 GUID），点「授权并添加」。

- **同步**：文件夹 `wellKnownFolderName` 映射本地角色；邮件取完整 MIME
  （`/messages/{id}/$value`）复用同一解析管线；增量用 `messages/delta`，
  deltaLink 断链自动重建。
- **发送**：MIME 建草稿 + send，保留 `Message-ID` / `References` / `In-Reply-To`
  线程头，发送后自动落「已发送」；超大附件（>4MB）自动回退 JSON + 上传会话分片。

## 1.2 AI 工具（MCP）

插件通过 `mcp` 贡献点把邮件能力暴露给 DBX 内置 AI 与外部 `dbx` MCP 服务
（`external_tools: true`，工具分 `mailbox-read` / `mailbox-write` 两个工具集）。
`mcp/tools` 只做发现（无副作用），`mcp/call` 逐参数校验后复用 Sidecar 内部引擎：

| 工具 | 读写 | 说明 |
| --- | --- | --- |
| `folder_list` | read | 列出某账户全部文件夹与计数 |
| `mail_list` | read | 按文件夹/关键词列会话，`cursor`（`o<offset>`）+ `limit`（1-50）分页 |
| `mail_read` | read | 按消息或会话 id 返回纯文本正文（正文缺省时自动 HTML 转文本；按需取正文可走网络） |
| `mail_flag` | write | 设置/清除 已读、星标、已答复 |
| `mail_move` | write | 按角色名或文件夹 id 移动邮件（默认归档） |
| `mail_send` | write | 发送邮件；**两阶段**：先 `dryRun: true` 预览收件人/主题/正文（不入队），再去掉 dryRun 真正入发件箱 |

- 工具以插件自有账户为作用域；宿主注入的 `connectionId` 只接受、不透传。
- 凭据红线：参数与输出都不含密码/令牌；工具定义经断言校验不出现敏感词。
- `mail_send` 不支持附件；发送经发件箱队列投递并保留撤销窗口。

## 1.3 DBX 连接管理（connection-provider）

在 **DBX 宿主连接管理器**里也能创建、测试、连接、断开邮件账户
（`connection-provider` 贡献点，表单字段与插件内 `account/add` 参数对齐）：

- **账户类型 `kind`**：`imap`（IMAP/SMTP 服务器，含 host/port/加密/用户名/密码）或
  `graph`（Microsoft 365 / Outlook.com：邮箱 + Azure `clientId`，租户默认 `common`）。
- **Graph 双模式**（`auth_mode`）：
  - *浏览器授权*（默认）：`connection/connect` 只创建账户壳并提示授权；随后在插件
    工作台对该账户点「重新授权」完成 Microsoft 登录，文件夹与邮件即开始同步。
  - *纯后台（App-only）*：选 App-only 后追加 `clientSecret`（宿主密钥库托管）与
    `userId`（目标邮箱的 Object ID / UPN，留空回退邮箱地址）两栏；`connection/test`
    用 client_credentials 换一次应用令牌验证，`connection/connect` 成功后建
    `oauth2_app` 账户并立即后台首拉，全程无浏览器交互。
- **生命周期**：`connection/test` 校验（IMAP/SMTP 探测；Graph 浏览器态不做网络
  探测、纯后台态换令牌验证）；`connection/connect` 以 `connection.id` 为 key
  幂等建会话并复用既有同步引擎；`connection/disconnect` 停会话但保留本地缓存。
- **凭据语义**：宿主连接的密码与 client secret 只出现在生命周期请求里，驻留
  Sidecar **内存会话表**，绝不写入本地密钥库或 SQLite；`connection/disconnect`
  随会话一起丢弃。
- 与插件内向导并存：向导账户（含 Graph）继续可用；两种来源的账户以 `hostBound`
  标记区分，`connection/disconnect` 只解绑不移除数据。

## 2. 架构

```
┌──────────────── DBX 宿主（workbench iframe，CSP: connect-src 'none'）────────────────┐
│  React 19 + Zustand + Tailwind v4（单文件 bundle → ui/）                             │
│  window.dbxPlugin.invoke(method, params)  ── JSON-RPC 2.0 over stdin/stdout ──┐     │
└───────────────────────────────────────────────────────────────────────────────┼─────┘
                                                                                │
┌──────────────────────── Sidecar：backend（Rust / tokio）──────────────────────▼─────┐
│ main.rs     逐行读 JSON-RPC，握手 plugin/initialize，分发 40 余个方法               │
│ accounts.rs 服务商预设、账户增删改查、连接测试、文件夹发现                          │
│ hostconn.rs DBX 宿主连接生命周期：test/connect/disconnect、内存会话表、Graph 壳     │
│ mcp.rs      MCP 工具面：mcp/tools 纯发现 + mcp/call（列表/阅读/旗标/移动/发送）     │
│ imap.rs     async-imap + rustls（SSL/TLS 与 STARTTLS），UTF-7 文件夹名编解码         │
│ oauth.rs    OAuth2 授权码 + PKCE、127.0.0.1 回环回调、token 刷新与轮换回存           │
│ graph.rs    Microsoft Graph REST：folders/delta/$value/flags/move/MIME 发送          │
│ smtp.rs     lettre（SSL/TLS / STARTTLS / 明文），mail-builder 组装 MIME             │
│ mime.rs     mail-parser 解析、cid: 内联图转 data URL、附件与正文抽取、会话 key       │
│ sync.rs     增量同步、旗标双向、按需取正文、移动/删除、SMTP 投递 + APPEND 已发送     │
│ compose.rs  发件箱队列、撤销窗口、定时投递、草稿读写                                │
│ files.rs    目录浏览、附件保存、上传暂存、.eml 读写、同名去重                        │
│ net_guard.rs 出网白名单：图片代理的主机校验与逐跳重定向复核                          │
│ db.rs       SQLite（WAL）：11 张表、会话聚合查询、全文过滤                          │
│ secrets.rs  keyring → AES-GCM 本地加密兜底                                          │
│ tls.rs / stream.rs  进程级 rustls ClientConfig、Plain/TLS 流抽象                    │
└────────────────────────────────────────────────────────────────────────────────────┘
```

- 协议：DBX Plugin Protocol **v1**，`plugin/initialize` 返回 `protocolVersion: 1`，
  `plugin.id` / `plugin.version` 必须与 `manifest.json` 完全一致（否则宿主报
  `Sidecar identity or protocol does not match manifest`）。
- 清单 `permissions` 为空数组：宿主只接受 `host.network:<url>` 形式的网络权限，
  写 `host.copy` 之类会被判为 `Unsupported plugin Host API permission` 而拒绝加载。
  剪贴板/文件/网络都由 Sidecar 或页面自身完成。
- 前端不直接联网：远程图片、IMAP、SMTP 全部经由 Sidecar。

## 3. 数据与隐私

| 内容 | 位置 |
| --- | --- |
| 账户（不含密码）、文件夹、邮件头、旗标 | `<数据目录>/dbx-mail.db`（SQLite，WAL） |
| 正文缓存 | `dbx-mail.db` 的 `bodies` 表；原始邮件另存 `<数据目录>/raw/<account>/<mailbox>/<key>.eml` |
| 附件 | 不落库字节，只有 `attachments` 元数据；保存时从原始邮件解出直接写到用户选定目录 |
| 密码 | 系统凭据库；不可用时 `<数据目录>/secrets.bin`（AES-256-GCM，密钥 `secrets.key`） |
| 上传暂存 | `<数据目录>/tmp/`（撰写器附件先落这里，路径记入草稿） |
| 设置 | `dbx-mail.db` 的 `settings` 表（键值对） |

数据目录由宿主通过环境变量提供（`DBX_PLUGIN_DATA_DIR`），插件不写插件目录之外的位置。

## 4. 构建

前置：Node 20+、Rust stable、`cargo` 走镜像（`backend/.cargo/config.toml` 已配 rsproxy）。
Windows 本机若没有 MSVC Build Tools、或 MSVC 的 `link.exe` 被 Git 自带的 `link` 抢占，
在该机器上执行 `rustup default stable-x86_64-pc-windows-gnu` 切 gnu 工具链（机器级设置，
仓库不携带工具链文件——`rust-toolchain.toml` 对全平台生效，钉 windows-gnu 会让 macOS/Linux 上
cwd 位于 `backend/` 的 cargo 直接报非法工具链）。

```bash
npm install
npm run build          # vite → ui/（单文件 bundle，不可拆包）
npm run backend        # cargo build --release → backend/target/release/backend[.exe]
npm run sidecar-check  # 42 项协议冒烟（握手 / 设置 / 目录 / 列表 / 错误码 / Graph 全链路，需 debug 构建）
npm run pack           # scripts/package.mjs → release/<id>-<version>-<当前平台>.dbxp（本地只能打当前平台）
```

**多平台包由 CI 产出**：`.github/workflows/plugin-release.yml` 调用
`t8y2/dbx` 的复用工作流，在 `ubuntu-24.04` / `ubuntu-24.04-arm` / `windows-2022` /
`macos-15-intel` / `macos-15` 上各构建一次，产物挂到对应 Release。
发版流程：改版本号 → 打 tag → **Publish release**（draft 不触发 `release: published`）→
等 5 个 job 绿 → 下载 `release-candidates.json` 生成商店候选。
带原生 Sidecar 的插件无法产出 universal 包，官方 CLI 会直接拒绝跨平台打包。

- 多平台包由 CI 按 runner 原生 stable 工具链构建（Windows 为 MSVC），仓库不固定工具链。
- `npm run pack` 用本地打包器复刻官方 CLI 的产物格式（同样的目录布局、`checksums.json`、
  `artifact.json` 与改写后的 `manifest.json`）。之所以不用 `npm run pack:cli`：官方 CLI 以 `\\?\`
  前缀路径调用 cargo，MinGW 的 ld 无法处理，且没有跳过编译的开关。
- 若在装有 MSVC Build Tools 的机器上，`npm run pack:cli` 可直接使用。

本地预览界面（无宿主、无后端）：`npm run dev` 后访问 `http://localhost:5173/?demo=1`。

## 5. 目录结构

```
dbx-mail/
├─ manifest.json            # 插件清单（id / 版本 / 权限 / workbench 贡献）
├─ dbx-plugin.toml          # 打包配置（Rust backend + include 列表）
├─ backend/                 # Rust Sidecar（13 个模块 + 18 个单元测试）
├─ src/                     # 前端
│  ├─ lib/{bridge,rpc,demo,i18n,format,html}.ts
│  ├─ stores/{mail,composer,ui}.ts
│  └─ components/{Sidebar,ThreadList,Reader,Composer,SettingsDialog,AccountWizard,DirPicker,ShortcutsDialog}.tsx
├─ ui/                      # 构建产物（被宿主注入）
├─ scripts/sidecar-check.mjs
├─ screenshots/             # 文档截图
├─ docs/                    # 版本与部署说明
└─ licenses/                # 第三方许可证副本
```

## 6. 已知限制

- IMAP 账户只支持 IMAP/SMTP（不含 POP3、JMAP）；Microsoft 365 / Outlook.com 走 Graph API（见 §1.1）。Graph 账户的「已回复」标记为近似值（Graph 列表不提供 answered 旗标）。
- 富文本编辑器使用 `document.execCommand`，是本机 Chromium 的行为，功能足够但已废弃；后续可换可编辑层。
- 会话聚合依赖邮件头的 `References` / `In-Reply-To`，主题相同但无引用头的邮件不会合并。
- 搜索为 SQLite `LIKE`（不是 FTS5）：几千封邮件的本地缓存下足够快，超过十万封需要换 FTS。
- 附件不落库，只在需要时从原始邮件重新解出；若原始 `.eml` 被手工删除，附件将无法再取。
- 日历（`.ics`）只作为附件展示，不解析为日程。

## 6.1 服务商兼容性处理

中文文件夹名（如「已发送」「草稿箱」「垃圾邮件」）在 IMAP 线上协议里必须转成 **modified UTF-7**（[RFC 3501 §5.1.3](https://www.rfc-editor.org/rfc/rfc3501#section-5.1.3)），服务器只认 `&XfJT0ZAB-` 这种形式。本插件的处理规则：

- 数据库与界面里统一存**解码后的可读名字**；只在向服务器发 `SELECT` / `EXAMINE` / `STATUS` / `COPY` / `MOVE` / `APPEND` 前编码（`imap::encode_utf7`）。
- 若编码版被服务器拒绝，自动退回**原始名字**再试一次，兼容返回原始 UTF-8 名字的少数服务器。
- 能力列表含 `ID` 时先发 [RFC 2971](https://www.rfc-editor.org/rfc/rfc2971) 报到（`name` / `version` / `vendor`）。163、126 等国内邮箱不做这一步会以 `Unsafe Login` 拒绝 `SELECT`，表现为「连接测试通过但同步报错」。

## 6.2 正文渲染

邮件 HTML 先过 DOMPurify 白名单，再进入渲染策略（`src/lib/html.ts`）：

| 内容 | 处理 |
| --- | --- |
| 远程图片 | 默认拦截：**挂载前**就把 `src` 换成 1×1 透明 GIF + 斜纹占位框（按原始 `width`/`height` 定尺寸），原地址存进 `data-remote`，点「显示图片」后经本地代理取回转 data URL；声明尺寸 ≤2px 的按跟踪像素直接隐藏 |
| CSS 背景图 | `background-image: url(http…)` 与 `background="…"` 同样先拦后取 |
| `cid:` 内嵌图 | 后端解析时已转成 data URL，不需要联网 |
| 邮件 `<style>` | 整段抽出 → 所有选择器加 `.mail-html` 前缀（`@media` 递归）→ 去 `@import` → `position:fixed` 落回文档流 → 以 `textContent` 注入，避免邮件样式改到应用自身 |
| 排版语义 | Tailwind preflight 会清零 `*` 的 margin、去掉列表符号、把 `h1~h6` 拉平、把 `img` 变块级；`.mail-html` 内已逐项还原 |
| 深色模式 | 摘掉写死的 `color` / `background` / `border-color`（内联与 `<style>` 都摘），交给主题变量 |

- 「显示图片」当前是**会话级**开关：点一次之后，本会话打开的其它邮件也会自动取图。
  按邮件/按发件人的粒度留待后续版本。

回归脚本（需要 demo 富格式样例，`?demo=1&rich=1`）：

```bash
node scripts/render-check.mjs        # 拦截态 / 放开态截图与 DOM 断言
node scripts/render-check-dark.mjs   # 深色模式主色检查
node scripts/render-check-scroll.mjs # 长正文可滚动、不横向裁切（?rich=2 固定 700px 模板）
node scripts/render-check-folder.mjs # 自定义文件夹按需同步、同角色文件夹不重名
node scripts/sanitize-check.mjs      # 注入回归：阅读器与回复引用两条路径（?evil=1）
```

`sanitize-check` 用的是 `?demo=1&evil=1` 挂的恶意样例（`<img onerror>`、`javascript:` 链接、
`<iframe>`、`<svg onload>`、`<script>`、`<form>`、CSS 远程 `url()`），每条载荷都会写
`window.__dbxXss`，断言的是「标记没出现」而不是「标签被删掉」，改标签名绕不过去。

这些脚本通过 `scripts/browser.mjs` 解析 playwright 与 Chromium，不再写死本机路径。
自动探测失败时用环境变量指定：

```bash
PLAYWRIGHT_CORE=/path/to/playwright-core/index.mjs \
CHROMIUM_PATH=/path/to/chrome \
node scripts/sanitize-check.mjs
```

### 经 DavMail 连 Microsoft 365（OAuth 环境用）

企业租户关掉 basic auth 后，IMAP 只留 `AUTH=XOAUTH2`，插件拿密码登不进去。
可先用 DavMail 做本地协议翻译网关（IMAP/SMTP → EWS），再让插件连 `127.0.0.1`：

```bash
# 1) 解压 DavMail 到 .tools/davmail（需 jar + lib/，已在 .gitignore 里）
#    https://sourceforge.net/projects/davmail/files/davmail/
# 2) 一键登录：自动拉起网关 → 打印设备码并打开浏览器 → 登录成功后自动联调
node scripts/davmail-login.mjs --email you@contoso.com --check --keep-gateway
# 3) 单独跑联调（网关已在跑时）
node scripts/davmail-check.mjs --email you@contoso.com
# 4) 收工
node scripts/davmail-login.mjs --stop
```

- `davmail-login.mjs` 用 `-Duser.home` 把配置沙箱到 `.tools/davmail/home`，不碰真实家目录；
  只监听 `127.0.0.1`，并关闭本地 TLS（自签证书过不了 rustls 的根证书校验）。
- `davmail-check.mjs` 不经过宿主 UI，直接以子进程拉起 sidecar 跑
  `plugin/initialize → account/test → account/add → account/discover → mailbox/sync → mail/list → mail/open`。
- 插件里添加账户时填：IMAP `127.0.0.1:1143`、SMTP `127.0.0.1:1025`、加密方式**无**、密码随意（DavMail 自己走 OAuth）。

## 6.3 同步范围

- 默认 `primary`：只同步收件箱 / 已发送 / 草稿 / 存档；自定义文件夹（如「12306」「通行费」）不进同步队列。
- 打开一个从未同步过的文件夹（`uidValidity = 0`）会自动同步一次；列表空状态提供「同步此文件夹」按钮，
  头部刷新按钮在打开具体文件夹时也只同步该文件夹。
- 需要一次拉全，把「设置 → 同步范围」改成「全部文件夹（含自定义，不含垃圾/已删除）」。

## 7. 许可

- 本插件：GPL-3.0-or-later（见 `LICENSE`，FSF 标准文本，未作任何改动）。
- 版权归属：Copyright (C) 2026 mjj3610，见 `NOTICE`。
- 第三方组件、许可证全文与传递依赖中的非 MIT/Apache 组件见 `THIRD-PARTY-NOTICES.md` 与 `licenses/`。
