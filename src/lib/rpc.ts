/**
 * Sidecar 调用层
 *
 * 与 Rust 后端（backend/src/main.rs）的方法一一对应；前端只依赖这里的类型，
 * 以后换实现不用动 UI。所有调用都带超时，避免宿主卡死。
 */
import { bridge } from './bridge'
import { demoInvoke, demoEnabled } from './demo'

/* ---------------- 类型 ---------------- */

export interface Account {
  id: number
  email: string
  displayName: string
  username: string
  /** imap / graph（graph = Microsoft 365 / Outlook.com，走 Graph API） */
  provider: 'imap' | 'graph'
  /** password / oauth2 */
  authType: string
  tenantId: string
  clientId: string
  /** App-only 接入的目标邮箱（浏览器授权账户为空） */
  userId?: string
  imapHost: string
  imapPort: number
  imapSecurity: string
  smtpHost: string
  smtpPort: number
  smtpSecurity: string
  signature: string
  color: string
  syncEnabled: boolean
  syncInterval: number
  lastSyncAt: number
  lastError: string
}

export interface Mailbox {
  id: number
  accountId: number
  name: string
  delimiter: string
  role: string
  unread: number
  total: number
  uidValidity: number
  lastUid: number
  selectable: boolean
  sortOrder: number
}

export interface Participant {
  name: string
  email: string
}

export interface ThreadItem {
  threadId: string
  subject: string
  snippet: string
  date: number
  fromName: string
  fromAddr: string
  toAddrs: string[]
  accountId: number
  accountEmail: string
  accountColor: string
  mailboxId: number
  mailboxRole: string
  latestId: number
  count: number
  unread: number
  flagged: boolean
  hasAttachments: boolean
  answered: boolean
  participants: Participant[]
}

export interface MessageSummary {
  id: number
  accountId: number
  mailboxId: number
  mailboxRole: string
  uid: number | null
  messageId: string
  threadId: string
  subject: string
  fromName: string
  fromAddr: string
  toAddrs: string[]
  ccAddrs: string[]
  date: number
  size: number
  snippet: string
  seen: boolean
  flagged: boolean
  answered: boolean
  draft: boolean
  hasAttachments: boolean
  bodyLoaded: boolean
}

export interface Attachment {
  index: number
  filename: string
  mime: string
  size: number
  contentId: string
  inline: boolean
}

export interface MessageDetail extends MessageSummary {
  textBody: string
  htmlBody: string
  attachments: Attachment[]
  inReplyTo: string
  references: string[]
  accountEmail: string
  accountName: string
  signature: string
}

export interface DraftAttachment {
  name: string
  path: string
  mime: string
  size: number
}

export interface Draft {
  id: number
  accountId: number
  toAddrs: string[]
  ccAddrs: string[]
  bccAddrs: string[]
  subject: string
  bodyHtml: string
  attachments: DraftAttachment[]
  inReplyTo: string
  references: string[]
  updatedAt: number
  createdAt: number
}

export interface OutboxItem {
  id: number
  accountId: number
  accountEmail: string
  toAddrs: string[]
  ccAddrs: string[]
  bccAddrs: string[]
  subject: string
  bodyHtml: string
  attachments: DraftAttachment[]
  scheduledAt: number
  status: string
  error: string
  attempts: number
  createdAt: number
  sentAt: number
  inReplyTo: string
  references: string[]
}

export interface Settings {
  theme: 'system' | 'light' | 'dark'
  locale: 'zh-CN' | 'en'
  density: 'comfortable' | 'compact'
  remoteImages: boolean
  markReadOnOpen: boolean
  undoDelay: number
  syncInterval: number
  syncScope: 'primary' | 'all'
  initialLimit: number
  layout: 'split' | 'list'
  conversations: boolean
  pageSize: number
}

export interface ProviderPreset {
  key: string
  label: string
  domains: string[]
  /** imap / graph；graph 预设不含服务器字段，走 OAuth 授权 */
  provider: 'imap' | 'graph'
  imapHost: string
  imapPort: number
  imapSecurity: string
  smtpHost: string
  smtpPort: number
  smtpSecurity: string
  hint: string
}

export interface Counts {
  inboxUnread: number
  starred: number
  drafts: number
  outbox: number
}

export interface Bootstrap {
  version: string
  dataDir: string
  vault: 'keyring' | 'local'
  platform: string
  accounts: Account[]
  mailboxes: Mailbox[]
  settings: Settings
  counts: Counts
}

export interface DirEntry {
  name: string
  path: string
  size?: number
  mtime?: number
}

export interface DirListing {
  path: string
  parent: string
  entries: DirEntry[]
  roots: DirEntry[]
  shortcuts: DirEntry[]
  writable: boolean
}

export interface ThreadPage {
  items: ThreadItem[]
  offset: number
  limit: number
}

/* ---------------- 调用封装 ---------------- */

/** 默认超时：普通读写 */
const DEFAULT_TIMEOUT = 20_000
/** 网络相关（同步、取正文、保存附件） */
const NETWORK_TIMEOUT = 110_000
/** 发送邮件 */
const SEND_TIMEOUT = 120_000

export async function call<T>(method: string, params: object = {}, timeoutMs = DEFAULT_TIMEOUT): Promise<T> {
  const host = bridge()
  if (typeof host?.invoke === 'function') {
    try {
      return (await host.invoke(method, params, { timeoutMs })) as T
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      throw new Error(normalizeError(message) || `后端调用失败：${method}`)
    }
  }
  if (demoEnabled) {
    return demoInvoke(method, params) as Promise<T>
  }
  throw new Error('当前环境没有可用的原生后端（Sidecar），请在 DBX 中运行本插件')
}

function normalizeError(message: string): string {
  if (!message) return ''
  try {
    const parsed = JSON.parse(message)
    if (parsed && typeof parsed === 'object' && typeof parsed.message === 'string') {
      return parsed.message
    }
  } catch {
    /* 不是 JSON，直接用原文 */
  }
  return message
}

/** 探测 Sidecar 是否可用 */
export async function probeSidecar(): Promise<boolean> {
  try {
    const result = await call<{ ok: boolean }>('ping', {}, 4000)
    return result?.ok === true
  } catch {
    return false
  }
}

/* ---------------- 启动 ---------------- */

export function bootstrap(accountId?: number): Promise<Bootstrap> {
  return call('app/bootstrap', accountId ? { accountId } : undefined)
}

/** 宿主连接作用域：connectionId → 绑定账户（独立邮箱 tab），未绑定返回 null */
export function accountByConnection(connectionId: string): Promise<{ account: Account | null }> {
  return call('account/byConnection', { connectionId })
}

export function appInfo(): Promise<{ version: string; dataDir: string; vault: string; platform: string }> {
  return call('app/info')
}

/* ---------------- 账户 ---------------- */

export function accountPresets(): Promise<{ items: ProviderPreset[] }> {
  return call('account/presets')
}

export function accountAdd(payload: Record<string, unknown>): Promise<{ account: Account; vault: string; warnings: string[] }> {
  return call('account/add', payload, NETWORK_TIMEOUT)
}

export function accountUpdate(payload: Record<string, unknown>): Promise<{ account: Account }> {
  return call('account/update', payload)
}

export function accountRemove(id: number): Promise<{ deleted: boolean }> {
  return call('account/remove', { id })
}

export function accountTest(payload: Record<string, unknown>): Promise<{ ok: boolean; lines: string[] }> {
  return call('account/test', payload, NETWORK_TIMEOUT)
}

export function accountDiscover(id: number): Promise<{ mailboxes: Mailbox[] }> {
  return call('account/discover', { id }, NETWORK_TIMEOUT)
}

export interface OauthBegin {
  authorizeUrl: string
  state: string
  expiresIn: number
}

export type OauthStatus =
  | { status: 'pending' }
  | { status: 'done'; refreshToken: string }
  | { status: 'failed'; error: string }
  | { status: 'expired' }

/** 发起 OAuth 授权：返回浏览器授权 URL（回环回调由 sidecar 接收）。
 *  机密客户端传 clientSecret（仅驻内存，用于回调时的 code 交换）。 */
export function oauthBegin(
  tenantId: string,
  clientId: string,
  clientSecret?: string,
  accountId?: number,
): Promise<OauthBegin> {
  return call(
    'account/oauthBegin',
    {
      tenantId,
      clientId,
      ...(clientSecret ? { clientSecret } : {}),
      // 重新授权：带上 accountId 让后端取回存量 secret（机密客户端的 code 交换必需）
      ...(accountId ? { accountId } : {}),
    },
    NETWORK_TIMEOUT,
  )
}

export function oauthStatus(state: string): Promise<OauthStatus> {
  return call('account/oauthStatus', { state })
}

export function oauthCancel(state: string): Promise<{ ok: boolean }> {
  return call('account/oauthCancel', { state })
}

/* ---------------- 同步 ---------------- */

export function syncNow(params: { accountId?: number; mailboxId?: number; limit?: number } = {}): Promise<{
  reports: unknown[]
  errors: string[]
}> {
  return call('sync/now', params, NETWORK_TIMEOUT)
}

export function syncStatus(): Promise<{
  running: number[]
  progress: Array<{ accountId: number; mailbox: string; done: number; total: number }>
  log: Array<{ accountId: number; at: number; ok: boolean; message: string }>
}> {
  return call('sync/status')
}

/* ---------------- 邮件列表 ---------------- */

export function listThreads(params: {
  mailboxId?: number
  role?: string
  accountId?: number
  query?: string
  kind?: 'all' | 'unread' | 'flagged' | 'files'
  limit?: number
  offset?: number
}): Promise<ThreadPage> {
  return call('mail/list', params)
}

export function loadThread(accountId: number, threadId: string): Promise<{ items: MessageSummary[] }> {
  return call('mail/thread', { accountId, threadId })
}

export function openMessage(id: number, network = true): Promise<MessageDetail> {
  return call('mail/open', { id, network }, NETWORK_TIMEOUT)
}

export function setFlags(
  ids: number[],
  flags: { seen?: boolean; flagged?: boolean; answered?: boolean },
): Promise<{ ok: boolean }> {
  return call('mail/flags', { ids, ...flags }, NETWORK_TIMEOUT)
}

export function moveMessages(
  ids: number[],
  destination: { kind: 'archive' | 'trash' | 'junk' | 'mailbox'; mailboxId?: number },
): Promise<{ ok: boolean }> {
  return call('mail/move', { ids, destination }, NETWORK_TIMEOUT)
}

export function readPart(id: number, index: number): Promise<{ name: string; mime: string; size: number; dataBase64: string }> {
  return call('mail/part', { id, index }, NETWORK_TIMEOUT)
}

export function exportEml(ids: number[], dir = ''): Promise<{ paths: string[]; dir: string }> {
  return call('mail/export', { ids, dir }, NETWORK_TIMEOUT)
}

export function importEml(accountId: number, mailboxId: number, path: string): Promise<{ id: number }> {
  return call('mail/import', { accountId, mailboxId, path }, NETWORK_TIMEOUT)
}

export function fetchRemoteImage(url: string): Promise<{ mime: string; dataUrl: string }> {
  return call('mail/fetchImage', { url }, 40_000)
}

/* ---------------- 撰写 ---------------- */

export function sendMail(payload: {
  accountId: number
  to: string[]
  cc: string[]
  bcc: string[]
  subject: string
  html: string
  attachments: DraftAttachment[]
  inReplyTo?: string
  references?: string[]
  draftId?: number
  mode?: 'now' | 'delay' | 'schedule'
  delaySeconds?: number
  scheduledAt?: number
}): Promise<{ outboxId: number; scheduledAt: number; undoSeconds: number }> {
  return call('compose/send', payload, SEND_TIMEOUT)
}

export function listDrafts(): Promise<{ items: Draft[] }> {
  return call('draft/list')
}

export function saveDraft(draft: Draft): Promise<{ id: number; updatedAt: number }> {
  return call('draft/save', draft as unknown as Record<string, unknown>)
}

export function deleteDraft(id: number): Promise<{ ok: boolean }> {
  return call('draft/delete', { id })
}

export function listOutbox(): Promise<{ items: OutboxItem[] }> {
  return call('outbox/list')
}

export function cancelOutbox(id: number): Promise<{ canceled: boolean }> {
  return call('outbox/cancel', { id })
}

export function retryOutbox(id: number): Promise<{ ok: boolean }> {
  return call('outbox/retry', { id })
}

export function deleteOutbox(id: number): Promise<{ ok: boolean }> {
  return call('outbox/delete', { id })
}

/* ---------------- 联系人 ---------------- */

export function listContacts(): Promise<{ items: Participant[] }> {
  return call('contacts/list')
}

/* ---------------- 设置 ---------------- */

export function getSettings(): Promise<{ settings: Settings }> {
  return call('settings/get')
}

export function saveSettings(settings: Settings): Promise<{ settings: Settings }> {
  return call('settings/set', { settings })
}

/* ---------------- 文件系统 ---------------- */

export function listDirs(path = ''): Promise<DirListing> {
  return call('fs/dirs', { path })
}

export function listFiles(path = ''): Promise<{ items: DirEntry[] }> {
  return call('fs/files', { path })
}

export function readLocalFile(path: string): Promise<{
  name: string
  path: string
  mime: string
  size: number
  dataBase64: string
}> {
  return call('fs/readFile', { path }, NETWORK_TIMEOUT)
}

export function saveUpload(name: string, dataBase64: string): Promise<DraftAttachment> {
  return call('fs/saveUpload', { name, dataBase64 }, NETWORK_TIMEOUT)
}

export function defaultDir(): Promise<{ path: string }> {
  return call('fs/defaultDir')
}

export function saveAttachment(params: {
  messageId: number
  index: number
  dir: string
  filename: string
}): Promise<{ path: string }> {
  return call('fs/saveAttachment', params, NETWORK_TIMEOUT)
}
