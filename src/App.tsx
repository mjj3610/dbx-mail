import { useEffect, useMemo, useState } from 'react'
import { AlertTriangle, Loader2, Mail, Plus, RefreshCw } from 'lucide-react'
import { Toaster } from 'sonner'
import { bridge, awaitHostReady, hostAppearance, watchHostAppearance, onHostInit } from './lib/bridge'
import { t } from './lib/i18n'
import { useMail } from './stores/mail'
import { useUI } from './stores/ui'
import { openForward, openNewMessage, openReply } from './stores/composer'
import { Sidebar } from './components/Sidebar'
import { ThreadList } from './components/ThreadList'
import { Reader } from './components/Reader'
import { ComposerLayer } from './components/Composer'
import { SettingsDialog } from './components/SettingsDialog'
import { AccountWizard } from './components/AccountWizard'
import { ShortcutsDialog } from './components/ShortcutsDialog'
import { DirPickerDialog } from './components/DirPicker'

export default function App() {
  const mail = useMail()
  const ui = useUI()
  const [booted, setBooted] = useState(false)

  useEffect(() => {
    let cancelled = false
    // 先订阅宿主 init：context 经 postMessage 下发，可能晚于插件启动
    const offHostInit = onHostInit(() => {
      if (!cancelled) void useMail.getState().applyConnectionScope()
    })
    void (async () => {
      await awaitHostReady()
      if (cancelled) return
      await useMail.getState().init()
      // init 里已读过一次 context；若宿主 init 尚未到达，onInit 回调兜底
      await useMail.getState().applyConnectionScope()
      setBooted(true)
    })()
    return () => {
      cancelled = true
      offHostInit()
    }
  }, [])

  // 外观：跟随宿主 / 系统 / 手动
  useEffect(() => {
    const apply = () => {
      const appearance = resolveAppearance(mail.settings.theme)
      document.documentElement.dataset.theme = appearance
      useUI.getState().setAppearance(appearance)
    }
    apply()
    const stopHost = watchHostAppearance(apply)
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    media.addEventListener('change', apply)
    return () => {
      stopHost()
      media.removeEventListener('change', apply)
    }
  }, [mail.settings.theme])

  useEffect(() => {
    document.documentElement.dataset.density = mail.settings.density
    document.documentElement.lang = ui.locale
  }, [mail.settings.density, ui.locale])

  // 定时刷新发件箱与自动同步
  useEffect(() => {
    if (!booted || !mail.accounts.length) return
    const interval = window.setInterval(() => {
      if (mail.settings.syncInterval > 0 && !document.hidden) {
        void useMail.getState().sync()
      }
    }, Math.max(1, mail.settings.syncInterval) * 60_000)
    return () => window.clearInterval(interval)
  }, [booted, mail.accounts.length, mail.settings.syncInterval])

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null
      const typing =
        !!target &&
        (target.tagName === 'INPUT' ||
          target.tagName === 'TEXTAREA' ||
          target.tagName === 'SELECT' ||
          target.isContentEditable)
      const state = useMail.getState()
      const chrome = useUI.getState()

      if (event.key === 'Escape') {
        if (chrome.dialog) {
          chrome.closeDialog()
          return
        }
        if (chrome.dirPicker) {
          chrome.closeDirPicker(null)
          return
        }
        const composers = document.querySelectorAll('.composer')
        if (composers.length) return
        if (state.thread) state.closeThread()
        return
      }

      if (typing || event.metaKey || event.ctrlKey || event.altKey) return
      if (chrome.dialog || chrome.dirPicker) return

      switch (event.key) {
        case 'c':
          event.preventDefault()
          openNewMessage()
          break
        case 'r':
          if (state.detail) {
            event.preventDefault()
            openReply(state.detail)
          }
          break
        case 'a':
          if (state.detail) {
            event.preventDefault()
            openReply(state.detail, true)
          }
          break
        case 'f':
          if (state.detail) {
            event.preventDefault()
            openForward(state.detail)
          }
          break
        case 'j':
        case 'k': {
          event.preventDefault()
          const list = state.threads
          if (!list.length) return
          const index = list.findIndex(
            (item) => item.threadId === state.thread?.threadId && item.accountId === state.thread?.accountId,
          )
          const next = event.key === 'j' ? Math.min(list.length - 1, index + 1) : Math.max(0, index - 1)
          const target = list[index < 0 ? 0 : next]
          if (target) void state.openThread(target)
          break
        }
        case 'e':
          if (state.thread) {
            event.preventDefault()
            void state.archive([state.activeId ?? state.thread.latestId])
          }
          break
        case '#':
          if (state.thread) {
            event.preventDefault()
            void state.trash([state.activeId ?? state.thread.latestId])
          }
          break
        case 's':
          if (state.thread) {
            event.preventDefault()
            void state.setMessageFlag([state.activeId ?? state.thread.latestId], {
              flagged: !state.messages.some((message) => message.flagged),
            })
          }
          break
        case 'u':
          if (state.activeId) {
            event.preventDefault()
            void state.setMessageFlag([state.activeId], { seen: false })
          }
          break
        case '/':
          event.preventDefault()
          window.dispatchEvent(new CustomEvent('mail:search'))
          break
        default:
          break
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const appClass = useMemo(() => {
    const classes = ['app']
    if (ui.sidebarHidden) classes.push('sidebar-hidden')
    if (ui.appearance === 'dark') classes.push('dark')
    if (mail.thread) classes.push('mobile-reader')
    return classes.join(' ')
  }, [ui.sidebarHidden, ui.appearance, mail.thread])

  const needsAccount = booted && !mail.error && mail.accounts.length === 0

  return (
    <>
      <div className={appClass}>
        <Sidebar />
        {needsAccount ? (
          <Onboarding />
        ) : (
          <>
            <ThreadList />
            <Reader />
          </>
        )}
      </div>

      <ComposerLayer />
      {ui.dialog === 'settings' && <SettingsDialog />}
      {ui.dialog === 'wizard' && <AccountWizard />}
      {ui.dialog === 'shortcuts' && <ShortcutsDialog />}
      <DirPickerDialog />

      {!booted && (
        <div className="overlay" style={{ zIndex: 60 }}>
          <div style={{ display: 'grid', justifyItems: 'center', gap: 10, color: 'var(--ink-dim)' }}>
            <Loader2 size={22} className="spin" style={{ color: 'var(--accent)' }} />
            <span style={{ fontSize: 13 }}>{t(ui.locale, 'app.loading')}</span>
          </div>
        </div>
      )}

      <Toaster
        position="bottom-right"
        theme={ui.appearance}
        toastOptions={{
          style: {
            fontFamily: 'var(--font)',
            fontSize: '13px',
            borderRadius: '12px',
            background: 'var(--popover)',
            color: 'var(--ink)',
            border: '1px solid var(--border)',
          },
        }}
      />
    </>
  )
}

function Onboarding() {
  const locale = useUI((state) => state.locale)
  const openDialog = useUI((state) => state.openDialog)
  const error = useMail((state) => state.error)
  const init = useMail((state) => state.init)

  return (
    <main className="reader-pane" style={{ gridColumn: '2 / -1' }}>
      <div className="placeholder">
        <div className="placeholder-inner" style={{ maxWidth: 400 }}>
          <div className="placeholder-art">{error ? <AlertTriangle size={26} /> : <Mail size={26} />}</div>
          <div className="placeholder-title">
            {error ? t(locale, 'app.noBackend') : t(locale, 'app.noAccountTitle')}
          </div>
          <div className="placeholder-desc">
            {error ? error : t(locale, 'app.noAccountDesc')}
          </div>
          <div style={{ display: 'flex', gap: 8, marginTop: 10 }}>
            {error ? (
              <button className="btn btn-primary" onClick={() => void init()}>
                <RefreshCw size={14} /> {t(locale, 'action.retry')}
              </button>
            ) : (
              <button className="btn btn-primary" onClick={() => openDialog('wizard')}>
                <Plus size={14} /> {t(locale, 'nav.addAccount')}
              </button>
            )}
          </div>
        </div>
      </div>
    </main>
  )
}

/** 解析实际外观：手动优先，其次宿主，最后系统偏好 */
function resolveAppearance(theme: string): 'light' | 'dark' {
  if (theme === 'light' || theme === 'dark') return theme
  if (bridge()) return hostAppearance()
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}
