/**
 * 本地 Microsoft Graph / OAuth2 mock 服务器（冒烟测试专用）
 *
 * 模拟最小可用的 OAuth2 授权码 + PKCE 流程与 Graph API 子集：
 * authorize / token（含 refresh 轮换）/ me / mailFolders / messages 列表与
 * delta / $value 原文 / PATCH 旗标 / move / delete / MIME 建草稿 + send。
 * 另有 /__test/* 控制端点：注入增量变化、读取收到的 MIME、重置状态。
 *
 * token 的 expires_in 固定为 0：强制 sidecar 每次使用前刷新，
 * 从而真实走一遍 refresh_token 轮换（旧 refresh_token 会被拒绝）。
 *
 * 用法（一般由 sidecar-check.mjs 调起，无需手工运行）：
 *   const mock = await startMockGraph();  // ESM 导入
 *   // 或独立运行观察：node scripts/mock-graph.mjs
 */
import { createHash } from 'node:crypto'
import http from 'node:http'

/** 极简 RFC822 夹具 */
function mime({ subject, from, to, messageId, date = 'Mon, 1 Jan 2024 10:00:00 +0000', inReplyTo = '', references = '', html = '<p>正文</p>' }) {
  const headers = [
    `Message-ID: ${messageId}`,
    `Subject: ${subject}`,
    `From: ${from}`,
    `To: ${to}`,
    `Date: ${date}`,
    'MIME-Version: 1.0',
    'Content-Type: text/html; charset=utf-8',
  ]
  if (inReplyTo) headers.push(`In-Reply-To: ${inReplyTo}`)
  if (references) headers.push(`References: ${references}`)
  return `${headers.join('\r\n')}\r\n\r\n${html}\r\n`
}

const now = () => '2024-01-01T10:00:00Z'
let serial = 0
const nextId = () => `mock-${++serial}`

/** 文件夹与邮件夹具（reset 时重建） */
let state
function resetState() {
  serial = 0
  const m1 = { id: nextId(), conversation: 'conv-weekly', received: '2024-01-01T10:00:00Z', seen: false, flagged: false, hasAttachments: false,
    mime: mime({ subject: '第一封：项目周报', from: '张三 <zhangsan@example.com>', to: 'user@example.com', messageId: '<m1@example.com>', date: 'Mon, 1 Jan 2024 10:00:00 +0000' }) }
  const m2 = { id: nextId(), conversation: 'conv-weekly', received: '2024-01-01T09:00:00Z', seen: true, flagged: true, hasAttachments: false,
    mime: mime({ subject: '第二封：回复：项目周报', from: '李四 <lisi@example.com>', to: 'user@example.com', messageId: '<m2@example.com>', date: 'Mon, 1 Jan 2024 09:00:00 +0000', inReplyTo: '<m1@example.com>', references: '<m1@example.com>' }) }
  const m3 = { id: nextId(), conversation: 'conv-login', received: '2024-01-01T08:00:00Z', seen: false, flagged: false, hasAttachments: false,
    mime: mime({ subject: '第三封：登录提醒', from: '通知 <noreply@example.com>', to: 'user@example.com', messageId: '<m3@example.com>', date: 'Mon, 1 Jan 2024 08:00:00 +0000' }) }
  const sent = { id: nextId(), conversation: 'conv-sent', received: '2024-01-01T07:00:00Z', seen: true, flagged: false, hasAttachments: false,
    mime: mime({ subject: '发出：你好', from: '我 <user@example.com>', to: 'friend@example.com', messageId: '<s1@example.com>', date: 'Mon, 1 Jan 2024 07:00:00 +0000' }) }
  state = {
    folders: [
      { id: 'f-inbox', name: '收件箱', wellKnown: 'inbox', total: 3, unread: 2, messages: [m1, m2, m3] },
      { id: 'f-sent', name: '已发送', wellKnown: 'sentitems', total: 1, unread: 0, messages: [sent] },
      { id: 'f-drafts', name: '草稿', wellKnown: 'drafts', total: 0, unread: 0, messages: [] },
      { id: 'f-deleted', name: '已删除', wellKnown: 'deleteditems', total: 0, unread: 0, messages: [] },
      { id: 'f-junk', name: '垃圾邮件', wellKnown: 'junkemail', total: 0, unread: 0, messages: [] },
      { id: 'f-archive', name: '存档', wellKnown: 'archive', total: 0, unread: 0, messages: [] },
      { id: 'f-custom', name: '项目', wellKnown: '', total: 0, unread: 0, messages: [] },
    ],
    // delta 队列：注入的变化等下一次带 deltaLink 的请求消费
    deltaQueue: [],
    deltaConsumed: 0,
    // OAuth 状态
    challenges: new Map(), // code → {challenge, state}
    usedCodes: new Set(),
    refreshTokenSeq: 0,
    currentRefresh: null,
    usedRefresh: new Set(),
    currentAccess: null,
    accessSeq: 0,
    appSecret: 'app-secret-check', // client_credentials 校验用
    // 收到的发送内容（校验线程头）
    sentMimes: [],
    // sidecar 回调地址（authorize 时注册）
    lastAuthorizeQuery: null,
  }
  state.currentRefresh = null
}
resetState()

function folder(id) {
  return state.folders.find((item) => item.id === id)
}

/** 解码 MIME encoded-word（=?utf-8?B?...?= / =?utf-8?Q?...?=）；真实 Graph 的 subject 字段是解码后的明文 */
function decodeEncodedWord(raw) {
  return raw.replace(/=\?([^?]+)\?([bBqQ])\?([^?]*)\?=/g, (_, charset, encoding, text) => {
    try {
      if (encoding.toLowerCase() === 'b') {
        return Buffer.from(text, 'base64').toString(charset.toLowerCase() === 'utf-8' ? 'utf8' : charset)
      }
      const qEncoded = String(text).replace(/_/g, ' ').replace(/=([0-9A-Fa-f]{2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)))
      return Buffer.from(qEncoded, 'binary').toString(charset.toLowerCase() === 'utf-8' ? 'utf8' : charset)
    } catch {
      return text
    }
  })
}

function messageJson(message) {
  const header = (name) => message.mime.split('\r\n\r\n')[0].split('\r\n').find((line) => line.startsWith(`${name}:`)) || ''
  const from = header('From').slice(5).trim()
  const nameMatch = from.match(/^(.*)</)
  const addrMatch = from.match(/<(.*)>/)
  const to = header('To').slice(3).trim()
  return {
    id: message.id,
    // 真实 Graph 每封邮件都有 conversationId；未标注的夹具退化为独立会话
    conversationId: message.conversation || message.id,
    subject: decodeEncodedWord(header('Subject').slice(8).trim()),
    from: { emailAddress: { name: nameMatch ? nameMatch[1].trim() : from, address: addrMatch ? addrMatch[1] : from } },
    toRecipients: to.split(',').map((address) => ({ emailAddress: { address: address.trim() } })),
    receivedDateTime: message.received,
    isRead: message.seen,
    flag: { flagStatus: message.flagged ? 'flagged' : 'notFlagged' },
    hasAttachments: message.hasAttachments,
    bodyPreview: '正文',
  }
}

function baseUrl(request) {
  return `http://127.0.0.1:${request.socket.localPort}`
}

function json(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' })
  res.end(JSON.stringify(body))
}

/** PKCE S256 校验 */
function challengeOf(verifier) {
  return createHash('sha256').update(verifier).digest('base64url')
}

export function startMockGraph() {
  const server = http.createServer((request, response) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.on('end', () => {
      const body = Buffer.concat(chunks)
      const url = new URL(request.url, baseUrl(request))
      let path = url.pathname
      const base = baseUrl(request)
      const bearer = (request.headers.authorization || '').replace(/^Bearer\s+/, '')

      try {
        /* ---------- 控制端点 ---------- */
        if (path === '/__test/reset') {
          resetState()
          return json(response, 200, { ok: true })
        }
        if (path === '/__test/sent') {
          return json(response, 200, { items: state.sentMimes })
        }
        if (path === '/__test/deltaConsumed') {
          return json(response, 200, { count: state.deltaConsumed })
        }
        if (path === '/__test/inject') {
          // 注入增量：新邮件 / 删除 / 旗标变化（均按 folderId 归属到具体文件夹）
          const spec = JSON.parse(body.toString() || '{}')
          if (spec.newMessage) {
            const message = {
              id: nextId(),
              received: '2024-01-01T11:00:00Z',
              seen: false,
              flagged: false,
              hasAttachments: false,
              mime: mime({
                subject: spec.newMessage.subject || '第四封：增量新邮件',
                from: '增量 <delta@example.com>',
                to: 'user@example.com',
                messageId: `<${nextId()}@example.com>`,
                date: 'Mon, 1 Jan 2024 11:00:00 +0000',
              }),
            }
            folder(spec.folderId).messages.unshift(message)
            state.deltaQueue.push({ kind: 'upsert', folderId: spec.folderId, message })
          }
          if (spec.removeId) {
            let owner = null
            for (const item of state.folders) {
              if (item.messages.some((message) => message.id === spec.removeId)) owner = item.id
              item.messages = item.messages.filter((message) => message.id !== spec.removeId)
            }
            state.deltaQueue.push({ kind: 'remove', id: spec.removeId, folderId: owner || spec.folderId })
          }
          if (spec.flagId) {
            let owner = null
            for (const item of state.folders) {
              const target = item.messages.find((message) => message.id === spec.flagId)
              if (target) {
                target.flagged = Boolean(spec.flagged)
                owner = item.id
              }
            }
            state.deltaQueue.push({ kind: 'flag', id: spec.flagId, flagged: Boolean(spec.flagged), folderId: owner || spec.folderId })
          }
          return json(response, 200, { ok: true })
        }

        /* ---------- OAuth2 ---------- */
        if (path.endsWith('/oauth2/v2.0/authorize')) {
          const code = `code-${++serial}`
          state.challenges.set(code, {
            challenge: url.searchParams.get('code_challenge'),
            state: url.searchParams.get('state'),
          })
          state.lastAuthorizeQuery = Object.fromEntries(url.searchParams)
          return json(response, 200, { code, state: url.searchParams.get('state') })
        }
        if (path.endsWith('/oauth2/v2.0/token')) {
          const form = new URLSearchParams(body.toString())
          const grant = form.get('grant_type')
          if (grant === 'authorization_code') {
            const code = form.get('code')
            const entry = state.challenges.get(code)
            if (!entry || state.usedCodes.has(code)) {
              return json(response, 400, { error: 'invalid_grant', error_description: 'code 无效或已使用' })
            }
            if (challengeOf(form.get('code_verifier')) !== entry.challenge) {
              return json(response, 400, { error: 'invalid_grant', error_description: 'PKCE 校验失败' })
            }
            state.usedCodes.add(code)
            state.currentRefresh = `rt-${++state.refreshTokenSeq}`
            state.currentAccess = `at-${++state.accessSeq}`
            return json(response, 200, {
              token_type: 'Bearer',
              scope: 'offline_access Mail.ReadWrite Mail.Send User.Read',
              expires_in: 0, // 强制每次使用前刷新，验证轮换
              access_token: state.currentAccess,
              refresh_token: state.currentRefresh,
            })
          }
          if (grant === 'refresh_token') {
            const token = form.get('refresh_token')
            if (!token || token !== state.currentRefresh || state.usedRefresh.has(token)) {
              return json(response, 400, { error: 'invalid_grant', error_description: `refresh_token 已失效（${token}）` })
            }
            state.usedRefresh.add(token)
            state.currentRefresh = `rt-${++state.refreshTokenSeq}`
            state.currentAccess = `at-${++state.accessSeq}`
            return json(response, 200, {
              token_type: 'Bearer',
              scope: 'offline_access Mail.ReadWrite Mail.Send User.Read',
              expires_in: 0,
              access_token: state.currentAccess,
              refresh_token: state.currentRefresh,
            })
          }
          if (grant === 'client_credentials') {
            // 纯后台接入：校验 client_secret，发无 refresh_token 的应用令牌
            const secret = form.get('client_secret')
            if (!secret || secret !== state.appSecret) {
              return json(response, 401, { error: 'invalid_client', error_description: 'client_secret 校验失败' })
            }
            state.currentAccess = `at-${++state.accessSeq}`
            return json(response, 200, {
              token_type: 'Bearer',
              scope: 'https://graph.microsoft.com/.default',
              expires_in: 3600,
              access_token: state.currentAccess,
            })
          }
          return json(response, 400, { error: 'unsupported_grant_type' })
        }

        /* ---------- Graph：鉴权 ---------- */
        if (path.startsWith('/v1.0/')) {
          // 纯后台接入走 /v1.0/users/{id}/...，与 /me 同构，归一化后共用路由
          path = path.replace(/^\/v1\.0\/users\/[^/]+/, '/v1.0/me')
          if (bearer !== state.currentAccess) {
            return json(response, 401, { error: { code: 'InvalidAuthenticationToken', message: `token 过期（收到 ${bearer || '空'}，期望 ${state.currentAccess}）` } })
          }
          if (path === '/v1.0/me') {
            return json(response, 200, { displayName: '冒烟用户', userPrincipalName: 'user@example.com', id: 'me-1' })
          }
          if (path === '/v1.0/me/mailFolders') {
            return json(response, 200, { value: state.folders.map((item) => folderJson(item)) })
          }
          {
            const folderMatch = path.match(/^\/v1\.0\/me\/mailFolders\/([^/]+)(\/.*)?$/)
            if (folderMatch) {
              const item = folder(folderMatch[1])
              if (!item) return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '文件夹不存在' } })
              const rest = folderMatch[2] || ''
              if (rest === '' ) {
                return json(response, 200, folderJson(item))
              }
              if (rest === '/childFolders') {
                // 夹具为扁平结构：无子文件夹
                return json(response, 200, { value: [] })
              }
              if (rest === '/messages' && request.method === 'GET') {
                const top = Number(url.searchParams.get('$top') || 100)
                return json(response, 200, { value: item.messages.slice(0, top).map(messageJson) })
              }
              if (rest === '/messages/delta' && request.method === 'GET') {
                return handleDelta(item, url, base, response)
              }
            }
          }
          {
            // /v1.0/me/messages/{id}/... 或 /v1.0/me/messages（建草稿）
            const messageMatch = path.match(/^\/v1\.0\/me\/messages(?:\/([^/]+))?(\/[^?]*)?$/)
            if (messageMatch) {
              return handleMessage(messageMatch, request, body, url, response, base)
            }
          }
          return json(response, 404, { error: { code: 'NotFound', message: `未实现的端点：${path}` } })
        }
        return json(response, 404, { error: { code: 'NotFound', message: path } })
      } catch (error) {
        return json(response, 500, { error: { code: 'InternalServerError', message: String(error) } })
      }
    })
  })

  return new Promise((resolvePromise) => {
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      const base = `http://127.0.0.1:${address.port}`
      resolvePromise({
        base,
        authorizeBase: base,
        tokenBase: base,
        graphBase: base,
        close: () => server.close(),
        state,
      })
    })
  })
}

function folderJson(item) {
  return {
    id: item.id,
    displayName: item.name,
    wellKnownFolderName: item.wellKnown || null,
    totalItemCount: item.total,
    unreadItemCount: item.unread,
    childFolderCount: 0,
    isHidden: false,
  }
}

function handleDelta(item, url, base, response) {
  const deltaToken = url.searchParams.get('$deltatoken')
  const skipToken = url.searchParams.get('$skiptoken')
  if (!deltaToken && !skipToken) {
    // 基线第一页：全部消息 + nextLink 指向第二页（验证翻页）
    return json(response, 200, {
      value: item.messages.map(messageJson),
      '@odata.nextLink': `${base}/v1.0/me/mailFolders/${item.id}/messages/delta?$skiptoken=page2`,
    })
  }
  if (skipToken === 'page2') {
    // 基线第二页：空 + deltaLink
    return json(response, 200, { value: [], '@odata.deltaLink': `${base}/v1.0/me/mailFolders/${item.id}/messages/delta?$deltatoken=dl-1` })
  }
  // 增量页：消费该文件夹的变更（按 folderId 过滤）
  const changes = state.deltaQueue
    .map((change, index) => ({ change, index }))
    .filter(({ change }) => change.folderId === item.id)
    .map(({ change, index }) => ({ change, index }))
  state.deltaQueue = state.deltaQueue.filter((_, index) => !changes.some(({ index: used }) => used === index))
  const mapped = changes.map(({ change }) => {
    if (change.kind === 'upsert') {
      return { ...messageJson(change.message), parentFolderId: change.folderId }
    }
    if (change.kind === 'remove') {
      return { id: change.id, '@removed': { reason: 'deleted' } }
    }
    return {
      id: change.id,
      subject: '',
      receivedDateTime: now(),
      isRead: false,
      flag: { flagStatus: change.flagged ? 'flagged' : 'notFlagged' },
      hasAttachments: false,
    }
  })
  state.deltaConsumed += mapped.length
  return json(response, 200, {
    value: mapped,
    '@odata.deltaLink': `${base}/v1.0/me/mailFolders/${item.id}/messages/delta?$deltatoken=dl-2`,
  })
}

function handleMessage(messageMatch, request, body, url, response, base) {
  const id = messageMatch[1]
  const action = messageMatch[2] || ''

  if (!id) {
    // POST /v1.0/me/messages：JSON 建草稿 或 MIME 建草稿（text/plain body）
    const contentType = request.headers['content-type'] || ''
    const target = folder('f-drafts')
    if (contentType.startsWith('text/plain')) {
      const message = { id: nextId(), received: now(), seen: true, flagged: false, hasAttachments: false, mime: body.toString('utf8') }
      target.messages.push(message)
      target.total = target.messages.length
      return json(response, 201, { id: message.id })
    }
    const spec = JSON.parse(body.toString() || '{}')
    const message = { id: nextId(), received: now(), seen: true, flagged: false, hasAttachments: false, mime: mime({ subject: spec.subject || '(mock 草稿)', from: '我 <user@example.com>', to: (spec.toRecipients || []).map((entry) => entry.emailAddress?.address).join(', '), messageId: `<draft-${nextId()}@example.com>` }) }
    target.messages.push(message)
    target.total = target.messages.length
    return json(response, 201, { id: message.id })
  }

  if (action === '/$value' && request.method === 'PUT') {
    // PUT MIME 到草稿
    for (const item of state.folders) {
      const message = item.messages.find((entry) => entry.id === id)
      if (message) {
        message.mime = body.toString('utf8')
        return json(response, 200, {})
      }
    }
    return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '草稿不存在' } })
  }
  if (action === '/$value' && request.method === 'GET') {
    for (const item of state.folders) {
      const message = item.messages.find((entry) => entry.id === id)
      if (message) {
        response.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' })
        return response.end(message.mime)
      }
    }
    return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '邮件不存在' } })
  }
  if (action === '/send') {
    // send：草稿转入「已发送」（模拟 Graph 的 SaveToSentItems 行为）
    for (const item of state.folders) {
      const index = item.messages.findIndex((entry) => entry.id === id)
      if (index >= 0) {
        const [message] = item.messages.splice(index, 1)
        item.total = item.messages.length
        const sent = folder('f-sent')
        sent.messages.unshift(message)
        sent.total = sent.messages.length
        state.deltaQueue.push({ kind: 'upsert', folderId: 'f-sent', message })
        if (message.mime.includes('@dbx-mail')) {
          state.sentMimes.push(message.mime)
        }
        return json(response, 202, {})
      }
    }
    return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '草稿不存在' } })
  }
  if (action === '/move') {
    const spec = JSON.parse(body.toString() || '{}')
    for (const item of state.folders) {
      const index = item.messages.findIndex((entry) => entry.id === id)
      if (index >= 0) {
        const [message] = item.messages.splice(index, 1)
        item.total = item.messages.length
        const target = folder(spec.destinationId)
        target.messages.unshift(message)
        target.total = target.messages.length
        state.deltaQueue.push({ kind: 'remove', id, folderId: item.id })
        state.deltaQueue.push({ kind: 'upsert', folderId: target.id, message })
        return json(response, 201, { id: message.id })
      }
    }
    return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '邮件不存在' } })
  }
  if (action === '' && request.method === 'PATCH') {
    const spec = JSON.parse(body.toString() || '{}')
    for (const item of state.folders) {
      const message = item.messages.find((entry) => entry.id === id)
      if (message) {
        if (typeof spec.isRead === 'boolean') message.seen = spec.isRead
        if (spec.flag) message.flagged = spec.flag.flagStatus === 'flagged'
        return json(response, 200, messageJson(message))
      }
    }
    return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '邮件不存在' } })
  }
  if (action === '' && request.method === 'DELETE') {
    for (const item of state.folders) {
      const index = item.messages.findIndex((entry) => entry.id === id)
      if (index >= 0) {
        item.messages.splice(index, 1)
        item.total = item.messages.length
        return json(response, 204, {})
      }
    }
    return json(response, 404, { error: { code: 'ErrorItemNotFound', message: '邮件不存在' } })
  }
  return json(response, 404, { error: { code: 'NotFound', message: `未实现的消息操作：${action}` } })
}

// 独立运行时保持进程存活便于人工观察
if (process.argv[1] && process.argv[1].endsWith('mock-graph.mjs') && process.argv.length === 2) {
  const mock = await startMockGraph()
  console.log(`mock graph listening at ${mock.base}`)
}
