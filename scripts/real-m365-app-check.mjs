/**
 * 真实 M365「纯后台接入」端到端验收（只读）
 *
 * 用插件本体（debug sidecar）连真实租户：account/add（client_credentials）
 * → 文件夹发现 → 同步 → 读真实邮件。全程只读，不发送/不移动/不改旗标。
 *
 * 用法（凭据经环境变量注入，不写入任何文件）：
 *   M365_TENANT=<tenantId> M365_CLIENT=<clientId> \
 *   M365_CLIENT_SECRET=<secret> M365_USER=<email> \
 *   node scripts/real-m365-app-check.mjs
 */
import { spawn } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const tenant = process.env.M365_TENANT
const clientId = process.env.M365_CLIENT
const clientSecret = process.env.M365_CLIENT_SECRET
const userId = process.env.M365_USER

if (!tenant || !clientId || !clientSecret || !userId) {
  console.error('用法：M365_TENANT=<tenantId> M365_CLIENT=<clientId> M365_CLIENT_SECRET=<secret> M365_USER=<email> node scripts/real-m365-app-check.mjs')
  process.exit(2)
}

const binary = join(root, 'backend/target/debug/backend')
if (!existsSync(binary)) {
  console.error('找不到 debug 后端，请先运行 npm run backend:dev')
  process.exit(2)
}

const dataDir = mkdtempSync(join(tmpdir(), 'dbx-mail-real-'))
const failures = []
let nextId = 1
const pending = new Map()
let buffer = ''

const child = spawn(binary, [], {
  env: {
    // 不设 DBX_MAIL_TEST_ENDPOINTS / DBX_MAIL_AUTHORIZE_BASE / DBX_MAIL_GRAPH_BASE：
    // debug 构建在无覆盖时直连真实微软端点。
    // DBX_MAIL_SECRET_FILE=1：强制本地加密通道，避免无人值守时钥匙串弹窗挂起
    ...process.env,
    DBX_PLUGIN_DATA_DIR: dataDir,
    DBX_MAIL_SECRET_FILE: '1',
  },
  stdio: ['pipe', 'pipe', 'inherit'],
})

child.stdout.setEncoding('utf8')
child.stdout.on('data', (chunk) => {
  buffer += chunk
  let newline = buffer.indexOf('\n')
  while (newline >= 0) {
    const line = buffer.slice(0, newline).trim()
    buffer = buffer.slice(newline + 1)
    if (!line) {
      newline = buffer.indexOf('\n')
      continue
    }
    let message
    try {
      message = JSON.parse(line)
    } catch {
      continue
    }
    const entry = pending.get(message.id)
    if (!entry) continue
    pending.delete(message.id)
    if (message.error) entry.reject(new Error(message.error.message))
    else entry.resolve(message.result)
  }
})

function call(method, params = {}, timeoutMs = 60000) {
  const id = nextId++
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
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
  })
}

function check(name, condition, detail = '') {
  if (condition) console.log(`  ok   ${name}`)
  else {
    failures.push(`${name}${detail ? `：${detail}` : ''}`)
    console.log(`  FAIL ${name}${detail ? `：${detail}` : ''}`)
  }
}

try {
  console.log(`后端：${binary}（真实端点，只读验收）`)
  const init = await call('plugin/initialize', { host: { protocolVersions: [1], name: 'real-m365-check' } })
  check('握手成功', init?.protocolVersion === 1, JSON.stringify(init?.plugin))

  // 纯后台接入：无 oauthState，凭 clientSecret + userId；verify 即换应用令牌并探测文件夹
  const added = await call('account/add', {
    provider: 'graph',
    email: userId,
    displayName: '真实验收',
    tenantId: tenant,
    clientId,
    clientSecret,
    userId,
    verify: true,
  })
  const account = added?.account
  check('账户创建成功（oauth2_app）', account?.authType === 'oauth2_app' && account?.provider === 'graph', JSON.stringify(added?.account))
  check('文件夹发现正常（真实 /users/{id}/mailFolders）', (added?.warnings?.length ?? 0) === 0, JSON.stringify(added?.warnings))

  const mailboxes = await call('mailbox/list', { accountId: account.id })
  // mailbox/list 是全账户混合列表（跨账户设计），按 accountId 过滤
  const ownMailboxes = (mailboxes?.items || []).filter((item) => item.accountId === account.id)
  check('本地文件夹就位', ownMailboxes.length >= 5, String(ownMailboxes.length))

  // 验收限量首拉：limit 限制每文件夹拉取量（真实邮箱全量可能上千封 MIME，生产环境
  // 首拉需数分钟；验收只验证通道与数据可达，不追求全量）。
  // 周期巡检可能正占着同步槽做全量首拉（「正在同步中」）——等待重试。
  let sync = null
  for (let attempt = 0; attempt < 12; attempt++) {
    sync = await call('sync/now', { accountId: account.id, limit: 50 }, 900000)
    const busy = (sync?.errors || []).some((item) => /正在同步中/.test(String(item)))
    if (!busy) break
    console.log(`  ..   同步槽被巡检占用（第 ${attempt + 1} 次），30s 后重试`)
    await new Promise((resolve) => setTimeout(resolve, 30000))
  }
  check('真实同步无错误', Array.isArray(sync?.errors) && sync.errors.length === 0, JSON.stringify(sync))

  const inbox = ownMailboxes.find((item) => item.role === 'inbox') || ownMailboxes[0]
  const mails = await call('mail/list', { accountId: account.id, mailboxId: inbox?.id, limit: 10 })
  const subjects = (mails?.items || []).map((item) => item.subject)
  check('读到真实邮件', (mails?.items?.length ?? 0) > 0, subjects.slice(0, 5).join(' | '))

  const first = mails?.items?.[0]
  if (first) {
    // 正文按需取（mail/open → ensure_body → Graph $value）；latestId 即消息主键
    const detail = await call('mail/open', { id: first.latestId }, 120000)
    const body = String(detail?.textBody || detail?.htmlBody || '')
    check('真实正文可读', body.length > 0, `长度 ${body.length}`)
  }

  console.log(`\n验收邮箱：${userId}；本地数据目录（验收后删除）：${dataDir}`)
} catch (error) {
  failures.push(error instanceof Error ? error.message : String(error))
} finally {
  child.stdin.end()
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
  console.log('\n真实端到端验收（只读）全部通过')
}
