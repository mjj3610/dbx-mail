/**
 * Sidecar 协议冒烟测试
 *
 * 直接以子进程方式启动 Rust 后端，走 DBX Plugin Protocol v1（JSON-RPC over stdin/stdout），
 * 验证握手、数据库迁移、设置读写与文件系统浏览，不依赖 DBX 宿主。
 *
 * 用法：
 *   node scripts/sidecar-check.mjs                 # 自动在 backend/target 下找 debug/release 产物
 *   node scripts/sidecar-check.mjs --bin <path>    # 指定后端可执行文件
 */
import { spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { startMockGraph } from './mock-graph.mjs'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
const manifest = JSON.parse(readFileSync(join(root, 'manifest.json'), 'utf8'))

function findBinary() {
  const index = args.indexOf('--bin')
  if (index >= 0 && args[index + 1]) return resolve(args[index + 1])
  const candidates = [
    // release 优先：debug 目录里常留着很久以前编译的产物，会拿旧版本号误导判断
    join(root, 'backend/target/release/backend.exe'),
    join(root, 'backend/target/debug/backend.exe'),
    join(root, 'backend/target/release/backend'),
    join(root, 'backend/target/debug/backend'),
  ]
  const found = candidates.find((path) => existsSync(path))
  if (!found) {
    console.error('找不到后端可执行文件，请先运行 npm run backend:dev 或 --bin 指定路径')
    process.exit(2)
  }
  return found
}

const binary = findBinary()
const dataDir = mkdtempSync(join(tmpdir(), 'dbx-mail-check-'))
// mock Graph 需要先于 sidecar 启动：端点覆盖在 sidecar 进程内是启动期读取的。
// debug 构建才会生效；release 构建忽略覆盖（Graph 冒烟节会检测并跳过）。
const mock = await startMockGraph()
const child = spawn(binary, [], {
  env: {
    ...process.env,
    DBX_PLUGIN_DATA_DIR: dataDir,
    RUST_BACKTRACE: '1',
    DBX_MAIL_TEST_ENDPOINTS: '1',
    DBX_MAIL_AUTHORIZE_BASE: mock.authorizeBase,
    DBX_MAIL_GRAPH_BASE: mock.graphBase,
  },
  stdio: ['pipe', 'pipe', 'pipe'],
})

let buffer = ''
const pending = new Map()
let nextId = 1
const failures = []

child.stdout.setEncoding('utf8')
child.stdout.on('data', (chunk) => {
  buffer += chunk
  let newline = buffer.indexOf('\n')
  while (newline >= 0) {
    const line = buffer.slice(0, newline).trim()
    buffer = buffer.slice(newline + 1)
    if (line) handle(line)
    newline = buffer.indexOf('\n')
  }
})

child.stderr.setEncoding('utf8')
child.stderr.on('data', (chunk) => process.stderr.write(`[backend] ${chunk}`))

function handle(line) {
  let message
  try {
    message = JSON.parse(line)
  } catch {
    failures.push(`后端输出了非 JSON 内容：${line.slice(0, 120)}`)
    return
  }
  const entry = pending.get(message.id)
  if (!entry) return
  pending.delete(message.id)
  if (message.error) entry.reject(new Error(message.error.message))
  else entry.resolve(message.result)
}

function call(method, params = {}, timeoutMs = 15000) {
  const id = nextId++
  const payload = JSON.stringify({ jsonrpc: '2.0', id, method, params })
  return new Promise((resolvePromise, rejectPromise) => {
    const timer = setTimeout(() => {
      pending.delete(id)
      rejectPromise(new Error(`${method} 超时`))
    }, timeoutMs)
    pending.set(id, {
      resolve: (value) => {
        clearTimeout(timer)
        resolvePromise(value)
      },
      reject: (error) => {
        clearTimeout(timer)
        rejectPromise(error)
      },
    })
    child.stdin.write(`${payload}\n`)
  })
}

/** mcp/call 的 CallToolResult → 解出 content[0].text 里的 JSON payload */
function parseMcpText(result) {
  try {
    return JSON.parse(result?.content?.[0]?.text || 'null')
  } catch {
    return null
  }
}

function check(name, condition, detail = '') {
  if (condition) {
    console.log(`  ok   ${name}`)
  } else {
    failures.push(`${name}${detail ? `：${detail}` : ''}`)
    console.log(`  FAIL ${name}${detail ? `：${detail}` : ''}`)
  }
}

try {
  console.log(`后端：${binary}`)
  console.log(`数据目录：${dataDir}`)

  const init = await call('plugin/initialize', {
    host: { protocolVersions: [1], name: 'sidecar-check' },
  })
  check('握手返回协议版本 1', init?.protocolVersion === 1, JSON.stringify(init))
  check('插件标识与清单一致', init?.plugin?.id === manifest.id, init?.plugin?.id)
  check(
    `插件版本与清单一致（${manifest.version}）`,
    init?.plugin?.version === manifest.version,
    init?.plugin?.version,
  )

  const ping = await call('ping')
  check('ping', ping?.ok === true)

  const boot = await call('app/bootstrap')
  check('bootstrap 返回账户列表', Array.isArray(boot?.accounts))
  check('bootstrap 返回默认设置', boot?.settings?.pageSize === 60, JSON.stringify(boot?.settings))
  check('bootstrap 返回密码保管方式', ['keyring', 'local'].includes(boot?.vault), boot?.vault)

  const presets = await call('account/presets')
  check('账户预设包含 Gmail', presets?.items?.some((item) => item.key === 'gmail'))

  const saved = await call('settings/set', { settings: { ...boot.settings, theme: 'dark', undoDelay: 20 } })
  check('设置写入', saved?.settings?.theme === 'dark' && saved?.settings?.undoDelay === 20)

  const reloaded = await call('settings/get')
  check('设置持久化', reloaded?.settings?.theme === 'dark' && reloaded?.settings?.undoDelay === 20)

  const dirs = await call('fs/dirs', {})
  check('目录浏览返回快捷入口', Array.isArray(dirs?.shortcuts) && dirs.shortcuts.length > 0)

  const defaultDir = await call('fs/defaultDir')
  check('默认导出目录存在', typeof defaultDir?.path === 'string' && defaultDir.path.length > 0)

  const list = await call('mail/list', { role: 'inbox', limit: 20 })
  check('空邮箱列表可查询', Array.isArray(list?.items) && list.items.length === 0)

  const outbox = await call('outbox/list')
  check('发件箱可查询', Array.isArray(outbox?.items))

  const unknown = await call('not/a/method').then(
    () => null,
    (error) => error,
  )
  check('未知方法返回错误', unknown instanceof Error && /未实现/.test(unknown.message), String(unknown))

  // 端点覆盖仅在 debug 构建生效：探测一次 oauthBegin 指向，供各 mock 用例节判定跳过
  const mockProbe = await call('account/oauthBegin', { tenantId: 'common', clientId: 'mock-client' })
  const mockActive = String(mockProbe?.authorizeUrl || '').startsWith(mock.authorizeBase)

  /* ---------------- Microsoft 365 / Outlook.com（Graph API）全链路 ----------------
   * 端点覆盖仅在 debug 构建生效；release 构建忽略覆盖，检测到后跳过本节，
   * 避免把测试流量打到真实微软端点。 */
  try {
    console.log(`mock graph：${mock.base}`)

    const presetsGraph = await call('account/presets')
    check('预设包含 msgraph', presetsGraph?.items?.some((item) => item.key === 'msgraph' && item.provider === 'graph'))

    const begin = await call('account/oauthBegin', { tenantId: 'common', clientId: 'mock-client' })
    const authorizeUrl = String(begin?.authorizeUrl || '')
    if (!mockActive) {
      // release 构建不编译端点覆盖分支：授权 URL 指向真实微软端点，按设计跳过该节
      console.log('  SKIP Graph 冒烟：release 构建不含端点覆盖（需要 debug 构建：npm run backend:dev）')
    } else {
      check('oauthBegin 返回授权 URL 与 state', Boolean(begin?.state) && authorizeUrl.includes('code_challenge_method=S256'))

      // 充当浏览器：拿一次性 code，再回调 sidecar 的回环监听
      const authorize = await fetch(authorizeUrl).then((res) => res.json())
      check('mock authorize 发放一次性 code', Boolean(authorize?.code))
      const redirectUri = new URL(authorizeUrl).searchParams.get('redirect_uri')
      const callback = await fetch(`${redirectUri}?code=${authorize.code}&state=${authorize.state}`)
      check('回环回调返回完成页', callback.status === 200 && (await callback.text()).includes('授权完成'))

      // token 交换在后台异步完成，轮询等待
      let status = null
      for (let attempt = 0; attempt < 25; attempt++) {
        status = await call('account/oauthStatus', { state: begin.state })
        if (status?.status !== 'pending') break
        await new Promise((resolvePromise) => setTimeout(resolvePromise, 200))
      }
      check('oauthStatus 报告完成', status?.status === 'done', JSON.stringify(status))

      const added = await call('account/add', {
        provider: 'graph',
        email: 'user@example.com',
        displayName: '冒烟用户',
        tenantId: 'common',
        clientId: 'mock-client',
        oauthState: begin.state,
        verify: false,
      })
      const graphAccount = added?.account
      check('Graph 账户创建成功', graphAccount?.provider === 'graph' && graphAccount?.authType === 'oauth2', JSON.stringify(added?.account))
      check('文件夹发现正常', added?.warnings?.length === 0, JSON.stringify(added?.warnings))

      // 首次同步：文件夹引导 + 最新 N 封 + delta 基线
      const sync1 = await call('sync/now', { accountId: graphAccount.id }, 30000)
      check('Graph 首次同步无错误', sync1?.errors?.length === 0, JSON.stringify(sync1?.errors))
      check('首次同步发现文件夹', sync1?.reports?.[0]?.mailboxes === 7, JSON.stringify(sync1?.reports))

      const foldersAfter = await call('mailbox/list', {})
      const inbox = foldersAfter?.items?.find((item) => item.role === 'inbox' && item.accountId === graphAccount.id)
      const sent = foldersAfter?.items?.find((item) => item.role === 'sent' && item.accountId === graphAccount.id)
      check('收件箱计数来自服务器', inbox?.total === 3 && inbox?.unread === 2, JSON.stringify(inbox))

      // mail/list 返回会话（m1/m2 同线程归并 → 2 个会话、共 3 封）
      const threads = await call('mail/list', { mailboxId: inbox.id, limit: 20 })
      check('收件箱会话归并（3 封 → 2 会话）', threads?.items?.length === 2, JSON.stringify(threads?.items?.map((item) => item.subject)))
      check(
        '会话内计数合计 3 封',
        (threads?.items || []).reduce((sum, item) => sum + (item.count || 0), 0) === 3,
        JSON.stringify(threads?.items?.map((item) => item.count)),
      )
      const opened = await call('mail/open', { id: threads.items[0].latestId })
      check('正文与线程头就位', opened?.htmlBody?.includes('正文') === true && opened?.messageId?.includes('example.com'), opened?.messageId)

      // 旗标推送（PATCH isRead / flag）：会话最新一封（第一封）→ 取消星标并标已读
      const flaggedThread = threads.items.find((item) => item.flagged)
      check('初始夹具包含星标会话', Boolean(flaggedThread))
      await call('mail/flags', { ids: [flaggedThread.latestId], flagged: false, seen: true })
      const mockFlagged = mock.state.folders.find((item) => item.id === 'f-inbox').messages.find((message) => message.id === 'mock-1')
      check('旗标已推送到 Graph', mockFlagged?.flagged === false && mockFlagged?.seen === true, JSON.stringify(mockFlagged))

      // 移动到「存档」：第一封（最新会话的最新一封）
      const archive = foldersAfter?.items?.find((item) => item.role === 'archive' && item.accountId === graphAccount.id)
      const moved = threads.items[0]
      check('待移动的是第一封', moved.subject === '第一封：项目周报', moved.subject)
      await call('mail/move', { ids: [moved.latestId], destination: { kind: 'mailbox', mailboxId: archive.id } })
      const archiveMessages = mock.state.folders.find((item) => item.id === 'f-archive').messages
      check('移动已推送到 Graph', archiveMessages.length === 1, JSON.stringify(archiveMessages.length))

      // 增量同步：注入新邮件 / 删除 / 旗标
      await fetch(`${mock.base}/__test/inject`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ folderId: 'f-inbox', newMessage: { subject: '第四封：增量新邮件' }, removeId: 'mock-3', flagId: 'mock-2', flagged: true }),
      })
      const sync2 = await call('sync/now', { accountId: graphAccount.id }, 30000)
      check('增量同步无错误', sync2?.errors?.length === 0, JSON.stringify(sync2?.errors))
      const mails2 = await call('mail/list', { mailboxId: inbox.id, limit: 20 })
      check('增量新邮件到达', mails2?.items?.some((item) => item.subject === '第四封：增量新邮件'), JSON.stringify(mails2?.items?.map((item) => item.subject)))
      check('远端删除已生效', mails2?.items?.every((item) => item.subject !== '第三封：登录提醒'), JSON.stringify(mails2?.items?.map((item) => item.subject)))
      check('旗标增量已应用', mails2?.items?.find((item) => item.subject === '第二封：回复：项目周报')?.flagged === true)

      // 发送：MIME 建草稿 + send → 自动落「已发送」（delaySeconds=0 绕过撤销窗口，仅测试用）
      await call(
        'compose/send',
        { accountId: graphAccount.id, to: ['friend@example.com'], subject: 'Graph 冒烟发送', html: '<p>你好</p>', mode: 'delay', delaySeconds: 0 },
        15000,
      )
      await new Promise((resolvePromise) => setTimeout(resolvePromise, 4500))
      const outbox = await call('outbox/list')
      const outboxItem = outbox?.items?.[0]
      check('发件箱投递成功', outboxItem?.status === 'sent', JSON.stringify({ status: outboxItem?.status, error: outboxItem?.error }))
      const sentMimes = await fetch(`${mock.base}/__test/sent`).then((res) => res.json())
      // mail-builder 会把中文主题编码为 RFC2047，按 Message-ID 域识别我们的 MIME
      const sentMime = sentMimes?.items?.find((item) => item.includes('@dbx-mail'))
      check('MIME 到达 mock 且保留 Message-ID', Boolean(sentMime) && /Message-ID:\s*<.+@dbx-mail>/.test(sentMime || ''), String(sentMime?.split('\r\n')[0]))

      // 再次同步：发送的邮件从「已发送」回流到本地缓存
      const sync3 = await call('sync/now', { accountId: graphAccount.id }, 30000)
      check('发送后同步无错误', sync3?.errors?.length === 0, JSON.stringify(sync3?.errors))
      const mails3 = await call('mail/list', { mailboxId: sent.id, limit: 20 })
      check('已发送回流本地缓存', mails3?.items?.some((item) => item.subject === 'Graph 冒烟发送'), JSON.stringify(mails3?.items?.map((item) => item.subject)))

      // refresh 轮换：mock 的 expires_in=0 强制每次刷新，旧 refresh_token 会被拒绝
      check('refresh_token 轮换生效', mock.state.refreshTokenSeq >= 3, String(mock.state.refreshTokenSeq))
      check('delta 基线翻页生效', (await fetch(`${mock.base}/__test/deltaConsumed`).then((res) => res.json())).count >= 0)
    }

    /* ---------------- M365 纯后台接入（client_credentials） ---------------- */
    {
      console.log('M365 纯后台接入')
      // 纯后台用例依赖 mock 令牌端点；release 构建按设计跳过该节（同 Graph 冒烟）
      if (!mockActive) {
        console.log('  SKIP M365 纯后台：release 构建不含端点覆盖（需要 debug 构建：npm run backend:dev）')
      } else {
      const missingUser = await call('account/add', {
        provider: 'graph',
        email: 'app@example.com',
        tenantId: 'common',
        clientId: 'mock-client',
        clientSecret: 'app-secret-check',
        verify: false,
      }).then(() => null, (error) => error)
      check('纯后台缺 userId 报错', missingUser instanceof Error && /userId|目标邮箱/.test(missingUser?.message || ''), String(missingUser?.message || missingUser))

      const badSecret = await call('account/add', {
        provider: 'graph',
        email: 'app@example.com',
        tenantId: 'common',
        clientId: 'mock-client',
        clientSecret: 'wrong-secret',
        userId: 'app@example.com',
        verify: true,
      }).then(() => null, (error) => error)
      check('client_secret 错误验证失败', badSecret instanceof Error && /后台接入验证失败/.test(badSecret?.message || ''), String(badSecret?.message || badSecret))

      const addedApp = await call('account/add', {
        provider: 'graph',
        email: 'app@example.com',
        displayName: '后台接入',
        tenantId: 'common',
        clientId: 'mock-client',
        clientSecret: 'app-secret-check',
        userId: 'app@example.com',
        verify: true,
      })
      check('纯后台账户创建成功', addedApp?.account?.authType === 'oauth2_app', JSON.stringify(addedApp?.account))
      check('后台模式文件夹发现正常', (addedApp?.warnings?.length ?? 0) === 0, JSON.stringify(addedApp?.warnings))
      const appMailboxes = await call('mailbox/list', { accountId: addedApp.account.id })
      check('后台账户文件夹就位', (appMailboxes?.items?.length ?? 0) >= 7, String(appMailboxes?.items?.length))
      const syncApp = await call('sync/now', { accountId: addedApp.account.id }, 60000)
      check('纯后台同步无错误', Array.isArray(syncApp?.errors) && syncApp.errors.length === 0, JSON.stringify(syncApp))
      // mailbox/list 是全账户混合列表（跨账户设计），按 accountId 过滤出本账户的 inbox
      const appInbox = appMailboxes?.items?.find((item) => item.role === 'inbox' && item.accountId === addedApp.account.id)
      const appMails = await call('mail/list', { accountId: addedApp.account.id, mailboxId: appInbox?.id, limit: 10 })
      check('后台账户读到 mock 邮件', (appMails?.items?.length ?? 0) > 0, JSON.stringify(appMails?.items?.length))
      }
    }

    /* ---------------- MCP 工具面（mcp/tools 纯发现 + mcp/call） ---------------- */
    {
      console.log('MCP 工具面')
      const tools = await call('mcp/tools', {})
      const names = (tools?.tools || []).map((item) => item.name)
      check(
        'mcp/tools 列出全部工具',
        ['folder_list', 'mail_list', 'mail_read', 'mail_flag', 'mail_move', 'mail_send'].every((name) => names.includes(name)),
        JSON.stringify(names),
      )
      check(
        '工具定义带 inputSchema（发现无副作用）',
        (tools?.tools || []).every((item) => item.inputSchema && typeof item.inputSchema === 'object'),
      )

      const unknown = await call('mcp/call', { tool: 'no_such_tool', arguments: {} }).then(
        () => null,
        (error) => error,
      )
      check('未知工具报 RPC 错误', unknown instanceof Error && /未知工具/.test(unknown?.message || ''), String(unknown?.message || unknown))

      // 数据用例依赖 mock 灌入的邮件（纯后台节）；release 构建无数据，按设计跳过
      if (!mockActive) {
        console.log('  SKIP MCP 数据用例：release 构建不含端点覆盖（需要 debug 构建：npm run backend:dev）')
      } else {
      const folders = parseMcpText(await call('mcp/call', { tool: 'folder_list', arguments: {} }))
      check('folder_list 列出已同步文件夹', Array.isArray(folders?.items) && folders.items.length >= 7, JSON.stringify(folders?.items?.length))

      const mails = parseMcpText(await call('mcp/call', { tool: 'mail_list', arguments: { folder: 'inbox', limit: 5 } }))
      check('mail_list 返回会话列表', Array.isArray(mails?.items) && mails.count > 0, JSON.stringify(mails?.count))
      const first = mails?.items?.[0]
      // mail_read 的 content 是纯文本正文（非 JSON payload），直接读 text
      const read = await call('mcp/call', { tool: 'mail_read', arguments: { messageId: first?.latestId } })
      const readText = read?.content?.[0]?.text
      check('mail_read 返回纯文本正文', typeof readText === 'string' && readText.length > 0, String(typeof readText))

      // 写操作走已验证的内部引擎；发送只做 dry_run 预览，不入发件箱
      const flag = await call('mcp/call', { tool: 'mail_flag', arguments: { messageId: first?.latestId, flagged: true } })
      check('mail_flag 设置星标', flag?.isError === false, JSON.stringify(flag))
      const dry = parseMcpText(await call('mcp/call', {
        tool: 'mail_send',
        arguments: { accountId: first?.accountId, to: ['peer@example.com'], subject: 'MCP 冒烟', body: 'hello', dryRun: true },
      }))
      check('mail_send dryRun 预览且不入队', dry?.dryRun === true && Array.isArray(dry?.to), JSON.stringify(dry))
      const outbox = await call('outbox/list')
      check('dryRun 未产生发件箱条目', !(outbox?.items || []).some((item) => item.subject === 'MCP 冒烟'), JSON.stringify(outbox?.items?.map((item) => item.subject)))
      }
    }

    /* ---------------- DBX 连接管理（connection-provider 生命周期） ---------------- */
    {
      console.log('DBX 连接管理')
      // 契约：test 用 {success,message} 表达失败，不抛 RPC error
      const missing = await call('connection/test', {})
      check('connection/test 缺 connection 报告失败', missing?.success === false, JSON.stringify(missing))

      const noClient = await call('connection/test', {
        provider: { id: 'com.mjj3610.mail.connection' },
        connection: { id: 'check-graph-0', external_config: { kind: 'graph', email: 'check@example.com' } },
      })
      check('Graph 连接缺 Client ID 报告失败', noClient?.success === false && /Client ID/.test(noClient?.message || ''), noClient?.message)
      const graphTest = await call('connection/test', {
        provider: { id: 'com.mjj3610.mail.connection' },
        connection: { id: 'check-graph-0', external_config: { kind: 'graph', email: 'check@example.com', client_id: 'mock-client' } },
      })
      check('Graph 连接测试无需网络', graphTest?.success === true, graphTest?.message)

      const connect = await call('connection/connect', {
        provider: { id: 'com.mjj3610.mail.connection' },
        connection: { id: 'check-graph-0', name: '检查账户', external_config: { kind: 'graph', email: 'check@example.com', client_id: 'mock-client' } },
      })
      check('Graph connect 成功并提示插件内授权', connect?.success === true && /授权/.test(connect?.message || ''), connect?.message)
      const connectAgain = await call('connection/connect', {
        provider: { id: 'com.mjj3610.mail.connection' },
        connection: { id: 'check-graph-0', external_config: { kind: 'graph', email: 'check@example.com', client_id: 'mock-client' } },
      })
      check('connect 幂等（重复调用成功）', connectAgain?.success === true, connectAgain?.message)

      const accounts = await call('account/list')
      const hostAccount = accounts?.items?.find((item) => item.email === 'check@example.com')
      check('宿主账户入列表（graph + hostBound）', hostAccount?.provider === 'graph' && hostAccount?.hostBound === true, JSON.stringify(hostAccount))

      const disconnect = await call('connection/disconnect', { connection: { id: 'check-graph-0' } })
      check('disconnect 成功', disconnect?.success === true, disconnect?.message)
      const disconnectAgain = await call('connection/disconnect', { connection: { id: 'check-graph-0' } })
      check('disconnect 幂等', disconnectAgain?.success === true, disconnectAgain?.message)
      const accountsAfter = await call('account/list')
      check('disconnect 后账户行保留且解除绑定', accountsAfter?.items?.some((item) => item.email === 'check@example.com' && item.hostBound === false), JSON.stringify(accountsAfter?.items?.map((item) => [item.email, item.hostBound])))

      // Graph 纯后台生命周期：test 换应用令牌、connect 建账户、disconnect；依赖 mock
      if (!mockActive) {
        console.log('  SKIP 纯后台连接生命周期：release 构建不含端点覆盖（需要 debug 构建：npm run backend:dev）')
      } else {
      const appParams = {
        provider: { id: 'com.mjj3610.mail.connection' },
        connection: {
          id: 'check-graph-app-0',
          name: '纯后台连接',
          external_config: {
            kind: 'graph',
            auth_mode: 'app',
            email: 'check-app@example.com',
            client_id: 'mock-client',
            user_id: 'appuser@example.com',
          },
          connection_secrets: { client_secret: 'app-secret-check' },
        },
      }
      const appTest = await call('connection/test', appParams)
      check('纯后台 test 换应用令牌成功', appTest?.success === true && /appuser@example\.com/.test(appTest?.message || ''), appTest?.message)
      const appTestBad = await call('connection/test', {
        ...appParams,
        connection: { ...appParams.connection, id: 'check-graph-app-1', connection_secrets: { client_secret: 'wrong-secret' } },
      })
      check('纯后台 test 错误 secret 报告失败', appTestBad?.success === false, appTestBad?.message)

      const appConnect = await call('connection/connect', appParams)
      check('纯后台 connect 成功', appConnect?.success === true && /纯后台/.test(appConnect?.message || ''), appConnect?.message)
      const appAccounts = await call('account/list')
      const appHostAccount = appAccounts?.items?.find((item) => item.email === 'check-app@example.com')
      check('纯后台账户 app_only 且已绑定', appHostAccount?.authType === 'oauth2_app' && appHostAccount?.hostBound === true, JSON.stringify(appHostAccount))

      // 连接作用域 RPC：connectionId → 账户；bootstrap 按账户收窄（独立邮箱 tab）
      const byConn = await call('account/byConnection', { connectionId: 'check-graph-app-0' })
      check(
        'account/byConnection 返回绑定账户',
        byConn?.account?.email === 'check-app@example.com' && byConn.account.authType === 'oauth2_app',
        JSON.stringify(byConn),
      )
      const byConnNone = await call('account/byConnection', { connectionId: 'never-connected' })
      check('account/byConnection 未绑定返回 null', byConnNone?.account === null, JSON.stringify(byConnNone))
      const scopedBoot = await call('app/bootstrap', { accountId: byConn?.account?.id })
      check(
        'bootstrap 按账户收窄',
        scopedBoot?.accounts?.length === 1 && scopedBoot.accounts[0].id === byConn?.account?.id,
        JSON.stringify(scopedBoot?.accounts?.map((item) => item.email)),
      )

      const appDisconnect = await call('connection/disconnect', { connection: { id: 'check-graph-app-0' } })
      check('纯后台 disconnect 成功', appDisconnect?.success === true, appDisconnect?.message)
      }

      // IMAP 失败路径：不可路由端点快速失败，不打外网
      const imapFail = await call('connection/test', {
        provider: { id: 'com.mjj3610.mail.connection' },
        connection: {
          id: 'check-imap-0',
          host: '127.0.0.1',
          port: 1,
          username: 'smoke-user',
          connection_secrets: { password: 'p' },
          external_config: { email: 'checker@example.com', smtp_host: '127.0.0.1', smtp_port: 1 },
        },
      })
      check('IMAP 连接测试失败路径', imapFail?.success === false, imapFail?.message)
    }
  } finally {
    mock.close()
  }
} catch (error) {
  failures.push(error instanceof Error ? error.message : String(error))
} finally {
  child.stdin.end()
  mock.close()
  setTimeout(() => {
    child.kill()
    try {
      rmSync(dataDir, { recursive: true, force: true })
    } catch {
      /* 清理失败不影响结果 */
    }
    if (failures.length > 0) {
      console.error('\n失败项：')
      for (const item of failures) console.error(` - ${item}`)
      process.exit(1)
    }
    console.log('\n全部通过')
    process.exit(0)
  }, 500)
}
