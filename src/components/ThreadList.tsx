import { useEffect, useRef, useState } from 'react'
import {
  Archive,
  ArrowLeft,
  ChevronDown,
  Clock3,
  Download,
  FileText,
  Inbox,
  Loader2,
  Mail,
  Paperclip,
  RefreshCw,
  Reply,
  Search,
  Star,
  Trash2,
  X,
} from 'lucide-react'
import { t } from '../lib/i18n'
import { avatarColor, displayName, formatListTime, initials } from '../lib/format'
import { useMail } from '../stores/mail'
import type { FilterKind, Scope } from '../stores/mail'
import { useUI, pickDirectory, pickFile } from '../stores/ui'
import { openDraft } from '../stores/composer'
import { cancelOutbox, deleteOutbox, retryOutbox } from '../lib/rpc'
import { toast } from 'sonner'

const FILTERS: Array<{ key: FilterKind; label: string }> = [
  { key: 'all', label: 'list.filter.all' },
  { key: 'unread', label: 'list.filter.unread' },
  { key: 'flagged', label: 'list.filter.flagged' },
  { key: 'files', label: 'list.filter.files' },
]

export function scopeTitle(scope: Scope, locale: string): string {
  switch (scope.kind) {
    case 'all-inbox':
      return t(locale, 'nav.allInbox')
    case 'starred':
      return t(locale, 'nav.starred')
    case 'outbox':
      return t(locale, 'outbox.title')
    case 'drafts':
      return t(locale, 'nav.drafts')
    case 'all':
      return t(locale, 'nav.all')
    case 'account': {
      const account = useMail.getState().accounts.find((item) => item.id === scope.accountId)
      return account?.email ?? t(locale, 'nav.accounts')
    }
    case 'mailbox': {
      const state = useMail.getState()
      const mailbox = state.mailboxes.find((item) => item.id === scope.mailboxId)
      if (!mailbox) return t(locale, 'nav.mailboxes')
      const keys: Record<string, string> = {
        inbox: 'nav.inbox',
        sent: 'nav.sent',
        drafts: 'nav.drafts',
        archive: 'nav.archive',
        junk: 'nav.junk',
        trash: 'nav.trash',
      }
      // 同角色多个文件夹时，重复的那几个用真实名字，否则两个「垃圾邮件」分不清
      const sameRole = state.mailboxes
        .filter((item) => item.accountId === mailbox.accountId && item.role === mailbox.role)
        .sort((left, right) => left.sortOrder - right.sortOrder)
      const duplicated = sameRole.findIndex((item) => item.id === mailbox.id) > 0
      return !duplicated && keys[mailbox.role] ? t(locale, keys[mailbox.role]) : mailbox.name
    }
    default:
      return t(locale, 'nav.inbox')
  }
}

export function ThreadList() {
  const mail = useMail()
  const setQuery = useMail((state) => state.setQuery)
  const locale = useUI((state) => state.locale)
  const [searchOpen, setSearchOpen] = useState(false)
  const [draftQuery, setDraftQuery] = useState(mail.query)
  const [menuOpen, setMenuOpen] = useState(false)
  const menuRef = useRef<HTMLDivElement | null>(null)
  const inputRef = useRef<HTMLInputElement | null>(null)

  /** 当前打开的是具体文件夹时返回它的 id：刷新只同步这一个，别把整账户重拉一遍 */
  const currentMailboxId = (): number | undefined =>
    mail.scope.kind === 'mailbox' ? mail.scope.mailboxId : undefined

  /** 未读数：打开具体文件夹时用该文件夹的，否则用全局收件箱未读 */
  const unreadBadge =
    mail.scope.kind === 'mailbox'
      ? (mail.mailboxes.find((item) => item.id === mail.scope.mailboxId)?.unread ?? 0)
      : mail.counts.inboxUnread

  useEffect(() => {
    if (!searchOpen) return
    inputRef.current?.focus()
  }, [searchOpen])

  useEffect(() => {
    const open = () => setSearchOpen(true)
    window.addEventListener('mail:search', open)
    return () => window.removeEventListener('mail:search', open)
  }, [])

  // 切换文件夹/账户会清空查询，输入框跟着同步，避免「框里有词、列表却是全部」
  const committedQuery = mail.query
  useEffect(() => {
    setDraftQuery(committedQuery)
  }, [committedQuery])

  useEffect(() => {
    if (!searchOpen) return
    const timer = window.setTimeout(() => setQuery(draftQuery.trim()), 380)
    return () => window.clearTimeout(timer)
  }, [draftQuery, searchOpen, setQuery])

  useEffect(() => {
    if (!menuOpen) return
    const onClick = (event: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) setMenuOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [menuOpen])

  const title = mail.query ? t(locale, 'search.results', { query: mail.query }) : scopeTitle(mail.scope, locale)
  const isDrafts = mail.scope.kind === 'drafts'
  const isOutbox = mail.scope.kind === 'outbox'
  const showFilters = !isDrafts && !isOutbox

  const closeSearch = () => {
    setSearchOpen(false)
    setDraftQuery('')
    mail.setQuery('')
  }

  const doImport = async () => {
    setMenuOpen(false)
    const path = await pickFile('', t(locale, 'action.import'))
    if (!path) return
    try {
      await mail.importEml(path)
      toast.success(t(locale, 'toast.imported'))
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error))
    }
  }

  const selectAll = () => {
    setMenuOpen(false)
    const ids = mail.threads.map((item) => item.latestId).filter((id) => !mail.checked.includes(id))
    for (const id of ids) mail.toggleCheck(id)
  }

  return (
    <section className="list-pane">
      {mail.demo && (
        <div className="demo-strip" title={t(locale, 'app.demoHint')}>
          <Star size={12} />
          {t(locale, 'app.demo')}
        </div>
      )}
      <div className="list-head">
        <div className="list-title-wrap">
          <h1 className="list-title">{title}</h1>
          <div className="list-meta">
            {isDrafts ? (
              <span>{t(locale, 'misc.of', { count: mail.drafts.length })}</span>
            ) : isOutbox ? (
              <span>{t(locale, 'misc.of', { count: mail.outboxItems.length })}</span>
            ) : (
              <>
                <span>{t(locale, 'misc.of', { count: mail.threads.length })}</span>
                {unreadBadge > 0 && mail.scope.kind !== 'starred' && (
                  <span>· {t(locale, 'list.unreadCount', { count: unreadBadge })}</span>
                )}
              </>
            )}
          </div>
        </div>
        <div className="capsule">
          <button
            className={`cap-btn ${searchOpen ? 'active' : ''}`}
            title={t(locale, 'action.search')}
            onClick={() => (searchOpen ? closeSearch() : setSearchOpen(true))}
          >
            <Search size={15} />
          </button>
          <button
            className="cap-btn"
            title={
              mail.syncing && mail.progress
                ? `${mail.progress.mailbox} ${mail.progress.done}/${mail.progress.total}`
                : t(locale, 'action.sync')
            }
            onClick={() => void mail.sync(currentMailboxId())}
            disabled={mail.syncing}
          >
            {mail.syncing ? <Loader2 size={15} className="spin" /> : <RefreshCw size={15} />}
            {mail.syncing && mail.progress && mail.progress.total > 0 && (
              <span style={{ marginLeft: 6, fontSize: 12 }}>
                {mail.progress.done}/{mail.progress.total}
              </span>
            )}
          </button>
          <span className="cap-sep" />
          <div style={{ position: 'relative' }} ref={menuRef}>
            <button className="cap-btn" title={t(locale, 'action.more')} onClick={() => setMenuOpen(!menuOpen)}>
              <ChevronDown size={15} />
            </button>
            {menuOpen && (
              <div className="menu" style={{ right: 0, top: 36 }}>
                <button onClick={selectAll}>
                  <Mail size={14} /> {t(locale, 'action.selectAll')}
                </button>
                <button onClick={doImport}>
                  <Download size={14} /> {t(locale, 'action.import')}
                </button>
                <div className="sep" />
                <button
                  onClick={() => {
                    setMenuOpen(false)
                    void mail.loadList(true)
                  }}
                >
                  <RefreshCw size={14} /> {t(locale, 'action.refresh')}
                </button>
              </div>
            )}
          </div>
        </div>
      </div>

      {searchOpen && (
        <div style={{ padding: '0 18px 8px' }} className="fade-in">
          <input
            ref={inputRef}
            className="input"
            placeholder={t(locale, 'search.placeholder')}
            value={draftQuery}
            onChange={(event) => setDraftQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Escape') closeSearch()
            }}
          />
        </div>
      )}

      {showFilters && (
        <div className="filter-row">
          {FILTERS.map((filter) => (
            <button
              key={filter.key}
              className={`chip ${mail.filter === filter.key ? 'active' : ''}`}
              onClick={() => mail.setFilter(filter.key)}
            >
              {t(locale, filter.label)}
            </button>
          ))}
        </div>
      )}

      {mail.checked.length > 0 && (
        <div className="selection-bar">
          <span className="grow">{t(locale, 'list.selected', { count: mail.checked.length })}</span>
          <button
            className="icon-btn"
            title={t(locale, 'action.archive')}
            style={{ color: 'inherit' }}
            onClick={() => void mail.archive(mail.checked)}
          >
            <Archive size={15} />
          </button>
          <button
            className="icon-btn"
            title={t(locale, 'action.delete')}
            style={{ color: 'inherit' }}
            onClick={() => void mail.trash(mail.checked)}
          >
            <Trash2 size={15} />
          </button>
          <button
            className="icon-btn"
            title={t(locale, 'action.export')}
            style={{ color: 'inherit' }}
            onClick={async () => {
              const dir = await pickDirectory('', t(locale, 'dir.title'))
              if (!dir) return
              try {
                const paths = await mail.exportEml(mail.checked, dir)
                toast.success(t(locale, 'toast.exported', { count: paths.length }))
                mail.clearCheck()
              } catch (error) {
                toast.error(error instanceof Error ? error.message : String(error))
              }
            }}
          >
            <Download size={15} />
          </button>
          <button
            className="icon-btn"
            title={t(locale, 'action.clearSelection')}
            style={{ color: 'inherit' }}
            onClick={() => mail.clearCheck()}
          >
            <X size={15} />
          </button>
        </div>
      )}

      <div className="list-body">
        {isOutbox ? (
          <OutboxList />
        ) : isDrafts ? (
          <DraftList />
        ) : mail.loadingList && mail.threads.length === 0 ? (
          <SkeletonList />
        ) : mail.threads.length === 0 ? (
          <div className="empty-list">
            <div className="art">
              {mail.query ? <Search size={22} /> : <Inbox size={22} />}
            </div>
            <div className="title">
              {mail.query ? t(locale, 'list.noResults') : t(locale, 'list.emptyTitle')}
            </div>
            <div className="desc">
              {mail.query ? t(locale, 'list.noResultsDesc') : t(locale, 'list.emptyDesc')}
            </div>
            {/* 自定义文件夹不在默认同步范围，给一个明确的入口，别让人以为坏了 */}
            {!mail.query && mail.scope.kind === 'mailbox' && (
              <button
                className="btn btn-sm"
                style={{ marginTop: 14 }}
                disabled={mail.syncing}
                onClick={() => void mail.sync(currentMailboxId())}
              >
                {mail.syncing ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
                {t(locale, 'action.syncFolder')}
              </button>
            )}
          </div>
        ) : (
          <>
            {mail.threads.map((thread) => {
              const active =
                mail.thread?.threadId === thread.threadId && mail.thread?.accountId === thread.accountId
              const checked = mail.checked.includes(thread.latestId)
              return (
                <button
                  key={`${thread.accountId}-${thread.threadId}`}
                  className={`thread-row ${active ? 'active' : ''} ${thread.unread > 0 ? 'unread' : ''}`}
                  style={checked ? { boxShadow: 'inset 0 0 0 2px var(--accent)' } : undefined}
                  onClick={(event) => {
                    if (event.ctrlKey || event.metaKey) {
                      mail.toggleCheck(thread.latestId)
                      return
                    }
                    void mail.openThread(thread)
                  }}
                >
                  <span className="row-avatar" style={{ background: avatarColor(thread.fromAddr) }}>
                    {initials(thread.fromName, thread.fromAddr)}
                  </span>
                  <span className="row-main">
                    <span className="row-top">
                      <span className="row-from">
                        {displayName(thread.fromName, thread.fromAddr) || t(locale, 'misc.unknownSender')}
                      </span>
                      <span className="row-time">{formatListTime(thread.date, locale)}</span>
                    </span>
                    <span className="row-subject">
                      {thread.count > 1 && (
                        <span className="tag" style={{ flex: 'none' }}>
                          {thread.count}
                        </span>
                      )}
                      <span
                        style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                      >
                        {thread.subject || t(locale, 'misc.noSubject')}
                      </span>
                    </span>
                    <span className="row-snippet">{thread.snippet}</span>
                    {(thread.hasAttachments || thread.answered || thread.accountId > 0) && (
                      <span className="row-tags">
                        {thread.hasAttachments && (
                          <span className="tag">
                            <Paperclip size={10} /> {t(locale, 'misc.attachment')}
                          </span>
                        )}
                        {thread.answered && (
                          <span className="tag">
                            <Reply size={10} /> {t(locale, 'action.reply')}
                          </span>
                        )}
                        {mail.accounts.length > 1 && (
                          <span className="tag" title={thread.accountEmail}>
                            <span
                              style={{
                                width: 6,
                                height: 6,
                                borderRadius: 3,
                                background: thread.accountColor,
                              }}
                            />
                            {thread.accountEmail.split('@')[0]}
                          </span>
                        )}
                      </span>
                    )}
                  </span>
                  <span className="row-flags">
                    <span
                      role="button"
                      tabIndex={-1}
                      className={`star-btn ${thread.flagged ? 'on' : ''}`}
                      onClick={(event) => {
                        event.stopPropagation()
                        void mail.setMessageFlag([thread.latestId], { flagged: !thread.flagged })
                      }}
                    >
                      <Star size={14} fill={thread.flagged ? 'currentColor' : 'none'} />
                    </span>
                    {thread.unread > 0 && <span className="unread-dot" />}
                  </span>
                </button>
              )
            })}
            {mail.more && (
              <div style={{ padding: '4px 10px 8px' }}>
                <button
                  className="btn btn-ghost"
                  style={{ width: '100%' }}
                  onClick={() => void mail.loadList(false)}
                  disabled={mail.loadingList}
                >
                  {mail.loadingList ? <Loader2 size={14} className="spin" /> : <ChevronDown size={14} />}
                  {t(locale, 'list.loadMore')}
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </section>
  )
}

function SkeletonList() {
  return (
    <div>
      {[0, 1, 2, 3, 4, 5].map((index) => (
        <div className="skeleton-row" key={index}>
          <div className="sk" style={{ width: 34, height: 34, borderRadius: 17 }} />
          <div style={{ display: 'grid', gap: 7 }}>
            <div className="sk" style={{ width: `${40 + ((index * 13) % 30)}%`, height: 10 }} />
            <div className="sk" style={{ width: '85%', height: 10 }} />
            <div className="sk" style={{ width: '65%', height: 9 }} />
          </div>
        </div>
      ))}
    </div>
  )
}

function DraftList() {
  const { drafts, loadDrafts } = useMail()
  const locale = useUI((state) => state.locale)

  useEffect(() => {
    void loadDrafts()
  }, [loadDrafts])

  if (!drafts.length) {
    return (
      <div className="empty-list">
        <div className="art">
          <FileText size={22} />
        </div>
        <div className="title">{t(locale, 'list.emptyTitle')}</div>
        <div className="desc">{t(locale, 'list.emptyDesc')}</div>
      </div>
    )
  }

  return (
    <>
      {drafts.map((draft) => (
        <button key={draft.id} className="thread-row" onClick={() => openDraft(draft)}>
          <span className="row-avatar" style={{ background: 'var(--ink-faint)' }}>
            <FileText size={15} />
          </span>
          <span className="row-main">
            <span className="row-top">
              <span className="row-from">{t(locale, 'misc.draft')}</span>
              <span className="row-time">{formatListTime(draft.updatedAt, locale)}</span>
            </span>
            <span className="row-subject">{draft.subject || t(locale, 'misc.noSubject')}</span>
            <span className="row-snippet">
              {draft.toAddrs.length ? draft.toAddrs.join(', ') : t(locale, 'composer.to')}
            </span>
          </span>
          <span className="row-flags">
            {draft.attachments.length > 0 && (
              <span className="tag">
                <Paperclip size={10} /> {draft.attachments.length}
              </span>
            )}
          </span>
        </button>
      ))}
    </>
  )
}

function OutboxList() {
  const { outboxItems, loadOutbox, loadDrafts } = useMail()
  const locale = useUI((state) => state.locale)

  useEffect(() => {
    void loadOutbox()
    const timer = window.setInterval(() => void loadOutbox(), 4000)
    return () => window.clearInterval(timer)
  }, [loadOutbox])

  const statusLabel = (status: string) => {
    switch (status) {
      case 'sending':
        return t(locale, 'outbox.sending')
      case 'sent':
        return t(locale, 'outbox.sent')
      case 'failed':
        return t(locale, 'outbox.failed')
      case 'canceled':
        return t(locale, 'outbox.canceled')
      default:
        return t(locale, 'outbox.queued')
    }
  }

  if (!outboxItems.length) {
    return (
      <div className="empty-list">
        <div className="art">
          <Clock3 size={22} />
        </div>
        <div className="title">{t(locale, 'outbox.empty')}</div>
      </div>
    )
  }

  return (
    <>
      {outboxItems.map((item) => (
        <div key={item.id} className="thread-row" style={{ cursor: 'default' }}>
          <span
            className="row-avatar"
            style={{
              background:
                item.status === 'failed'
                  ? 'var(--red)'
                  : item.status === 'sent'
                    ? 'var(--green)'
                    : 'var(--accent)',
            }}
          >
            {item.status === 'failed' ? <X size={15} /> : item.status === 'sent' ? <ArrowLeft size={14} style={{ transform: 'rotate(135deg)' }} /> : <Clock3 size={14} />}
          </span>
          <span className="row-main">
            <span className="row-top">
              <span className="row-from">{item.subject || t(locale, 'misc.noSubject')}</span>
              <span className="row-time">{formatListTime(item.scheduledAt, locale)}</span>
            </span>
            <span className="row-subject" style={{ color: 'var(--ink-dim)' }}>
              {item.toAddrs.join(', ')}
            </span>
            <span className="row-tags">
              <span className={`tag ${item.status === 'failed' ? 'accent' : ''}`}>{statusLabel(item.status)}</span>
              {item.attachments.length > 0 && (
                <span className="tag">
                  <Paperclip size={10} /> {item.attachments.length}
                </span>
              )}
              {item.attempts > 1 && <span className="tag">×{item.attempts}</span>}
            </span>
            {item.error && (
              <span className="row-snippet" style={{ color: 'var(--red)' }}>
                {item.error}
              </span>
            )}
            {(item.status === 'queued' || item.status === 'failed') && (
              <span style={{ display: 'flex', gap: 6, marginTop: 6 }}>
                <button
                  className="btn btn-sm"
                  onClick={async () => {
                    try {
                      await cancelOutbox(item.id)
                      toast.success(t(locale, 'outbox.canceledToast'))
                      void loadOutbox()
                    } catch (error) {
                      toast.error(error instanceof Error ? error.message : String(error))
                    }
                  }}
                >
                  {t(locale, 'outbox.cancel')}
                </button>
                {item.status === 'failed' && (
                  <button
                    className="btn btn-sm"
                    onClick={async () => {
                      await retryOutbox(item.id)
                      void loadOutbox()
                    }}
                  >
                    {t(locale, 'outbox.retry')}
                  </button>
                )}
                <button
                  className="btn btn-sm btn-ghost"
                  onClick={async () => {
                    await deleteOutbox(item.id)
                    void loadOutbox()
                    void loadDrafts()
                  }}
                >
                  {t(locale, 'outbox.delete')}
                </button>
              </span>
            )}
          </span>
          <span className="row-flags" />
        </div>
      ))}
    </>
  )
}
