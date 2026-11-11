import { useEffect, useState } from 'react'
import { Check, Loader2, Plus, RefreshCw, ShieldCheck, Trash2, X } from 'lucide-react'
import { toast } from 'sonner'
import { t } from '../lib/i18n'
import { LOCALES } from '../lib/i18n'
import { displayName } from '../lib/format'
import * as api from '../lib/rpc'
import type { Account, Settings } from '../lib/rpc'
import { useMail } from '../stores/mail'
import { useUI } from '../stores/ui'
import type { SettingsTab } from '../stores/ui'

const TABS: Array<{ key: SettingsTab; label: string }> = [
  { key: 'general', label: 'settings.general' },
  { key: 'accounts', label: 'settings.accounts' },
  { key: 'sync', label: 'settings.sync' },
  { key: 'about', label: 'settings.about' },
]

export function SettingsDialog() {
  const locale = useUI((state) => state.locale)
  const tab = useUI((state) => state.settingsTab)
  const openDialog = useUI((state) => state.openDialog)
  const closeDialog = useUI((state) => state.closeDialog)
  const mail = useMail()
  const settings = mail.settings

  const save = async (patch: Partial<Settings>) => {
    try {
      await mail.saveSettings(patch)
    } catch (error) {
      toast.error(t(locale, 'settings.saveFailed', { message: error instanceof Error ? error.message : String(error) }))
    }
  }

  return (
    <div className="overlay" style={{ zIndex: 45 }}>
      <div className="dialog">
        <div className="dialog-head">
          <span className="dialog-title">{t(locale, 'settings.title')}</span>
          <button className="icon-btn" onClick={closeDialog}>
            <X size={16} />
          </button>
        </div>
        <div className="tabs">
          {TABS.map((item) => (
            <button
              key={item.key}
              className={`tab ${tab === item.key ? 'active' : ''}`}
              onClick={() => openDialog('settings', item.key)}
            >
              {t(locale, item.label)}
            </button>
          ))}
        </div>

        <div className="dialog-body">
          {tab === 'general' && (
            <>
              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.theme')}</div>
                </div>
                <div className="setting-control">
                  <div className="seg">
                    {(['system', 'light', 'dark'] as const).map((value) => (
                      <button
                        key={value}
                        className={`seg-btn ${settings.theme === value ? 'active' : ''}`}
                        onClick={() => void save({ theme: value })}
                      >
                        {t(locale, `settings.theme.${value}`)}
                      </button>
                    ))}
                  </div>
                </div>
              </div>

              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.language')}</div>
                </div>
                <div className="setting-control">
                  <select
                    className="select"
                    value={settings.locale}
                    onChange={(event) => void save({ locale: event.target.value as Settings['locale'] })}
                  >
                    {LOCALES.map((item) => (
                      <option key={item.key} value={item.key}>
                        {item.label}
                      </option>
                    ))}
                  </select>
                </div>
              </div>

              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.density')}</div>
                </div>
                <div className="setting-control">
                  <div className="seg">
                    {(['comfortable', 'compact'] as const).map((value) => (
                      <button
                        key={value}
                        className={`seg-btn ${settings.density === value ? 'active' : ''}`}
                        onClick={() => void save({ density: value })}
                      >
                        {t(locale, `settings.density.${value}`)}
                      </button>
                    ))}
                  </div>
                </div>
              </div>

              <Toggle
                label={t(locale, 'settings.remoteImages')}
                hint={t(locale, 'settings.remoteImagesHint')}
                value={settings.remoteImages}
                onChange={(value) => void save({ remoteImages: value })}
              />
              <Toggle
                label={t(locale, 'settings.markRead')}
                hint={t(locale, 'settings.markReadHint')}
                value={settings.markReadOnOpen}
                onChange={(value) => void save({ markReadOnOpen: value })}
              />

              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.pageSize')}</div>
                </div>
                <div className="setting-control">
                  <input
                    className="input"
                    type="number"
                    min={20}
                    max={300}
                    value={settings.pageSize}
                    onChange={(event) => void save({ pageSize: Number(event.target.value) || 60 })}
                  />
                </div>
              </div>
            </>
          )}

          {tab === 'accounts' && (
            <>
              {mail.accounts.length === 0 && (
                <div className="hint" style={{ marginBottom: 12 }}>
                  {t(locale, 'app.noAccountDesc')}
                </div>
              )}
              {mail.accounts.map((account) => (
                <AccountCard key={account.id} account={account} locale={locale} />
              ))}
              <button className="btn btn-primary" style={{ marginTop: 6 }} onClick={() => openDialog('wizard')}>
                <Plus size={14} /> {t(locale, 'settings.addAccount')}
              </button>
            </>
          )}

          {tab === 'sync' && (
            <>
              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.syncInterval')}</div>
                  <div className="setting-desc">{t(locale, 'settings.syncIntervalHint')}</div>
                </div>
                <div className="setting-control">
                  <input
                    className="input"
                    type="number"
                    min={0}
                    max={240}
                    value={settings.syncInterval}
                    onChange={(event) => void save({ syncInterval: Number(event.target.value) || 0 })}
                  />
                </div>
              </div>

              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.syncScope')}</div>
                </div>
                <div className="setting-control">
                  <select
                    className="select"
                    value={settings.syncScope}
                    onChange={(event) => void save({ syncScope: event.target.value as Settings['syncScope'] })}
                  >
                    <option value="primary">{t(locale, 'settings.scope.primary')}</option>
                    <option value="all">{t(locale, 'settings.scope.all')}</option>
                  </select>
                </div>
              </div>

              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.initialLimit')}</div>
                </div>
                <div className="setting-control">
                  <input
                    className="input"
                    type="number"
                    min={20}
                    max={2000}
                    value={settings.initialLimit}
                    onChange={(event) => void save({ initialLimit: Number(event.target.value) || 200 })}
                  />
                </div>
              </div>

              <div className="setting-row">
                <div className="setting-main">
                  <div className="setting-name">{t(locale, 'settings.undoDelay')}</div>
                  <div className="setting-desc">{t(locale, 'settings.undoHint')}</div>
                </div>
                <div className="setting-control">
                  <input
                    className="input"
                    type="number"
                    min={0}
                    max={60}
                    value={settings.undoDelay}
                    onChange={(event) => void save({ undoDelay: Number(event.target.value) || 0 })}
                  />
                </div>
              </div>

              <div style={{ marginTop: 14, display: 'flex', gap: 8 }}>
                <button className="btn" onClick={() => void mail.sync()} disabled={mail.syncing}>
                  {mail.syncing ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
                  {t(locale, 'settings.syncNow')}
                </button>
              </div>
            </>
          )}

          {tab === 'about' && (
            <div style={{ display: 'grid', gap: 10, fontSize: 13 }}>
              <Row label={t(locale, 'settings.version')} value={mail.version || '0.1.4'} />
              <Row label={t(locale, 'settings.dataDir')} value={mail.dataDir} mono />
              <Row
                label={t(locale, 'settings.vault')}
                value={
                  mail.vault === 'local'
                    ? t(locale, 'settings.vault.local')
                    : t(locale, 'settings.vault.keyring')
                }
              />
              <Row label="Platform" value={mail.platform} />
              <div className="hint" style={{ marginTop: 8, lineHeight: 1.7 }}>
                邮件副本、草稿与附件缓存都保存在本机数据目录；密码保存在系统凭据库（不可用时退化为本地加密文件）。
                附件、.eml 导出与目录浏览全部由本地 Rust 进程完成，页面本身没有文件系统权限。
              </div>
            </div>
          )}
        </div>

        <div className="dialog-foot">
          <span className="grow" />
          <button className="btn btn-primary" onClick={closeDialog}>
            {t(locale, 'action.close')}
          </button>
        </div>
      </div>
    </div>
  )
}

function Toggle({
  label,
  hint,
  value,
  onChange,
}: {
  label: string
  hint?: string
  value: boolean
  onChange: (value: boolean) => void
}) {
  return (
    <div className="setting-row">
      <div className="setting-main">
        <div className="setting-name">{label}</div>
        {hint && <div className="setting-desc">{hint}</div>}
      </div>
      <div className="setting-control">
        <button className={`switch ${value ? 'on' : ''}`} onClick={() => onChange(!value)} aria-pressed={value} />
      </div>
    </div>
  )
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div style={{ display: 'flex', gap: 12, alignItems: 'baseline' }}>
      <span style={{ width: 110, flex: 'none', color: 'var(--ink-faint)', fontSize: 12 }}>{label}</span>
      <span
        style={{
          flex: 1,
          minWidth: 0,
          overflowWrap: 'anywhere',
          fontFamily: mono ? 'var(--mono)' : undefined,
          fontSize: mono ? 12 : 13,
        }}
      >
        {value || '—'}
      </span>
    </div>
  )
}

function AccountCard({ account, locale }: { account: Account; locale: string }) {
  const updateAccount = useMail((state) => state.updateAccount)
  const removeAccount = useMail((state) => state.removeAccount)
  const sync = useMail((state) => state.sync)
  const [signature, setSignature] = useState(account.signature)
  const [confirming, setConfirming] = useState(false)
  const [busy, setBusy] = useState(false)
  const [reauthBusy, setReauthBusy] = useState(false)
  const isGraph = account.provider === 'graph'

  useEffect(() => {
    setSignature(account.signature)
  }, [account.signature])

  const persist = async () => {
    if (signature === account.signature) return
    setBusy(true)
    try {
      const base = {
        id: account.id,
        email: account.email,
        displayName: account.displayName,
        username: account.username,
        signature,
        syncEnabled: account.syncEnabled,
        syncInterval: account.syncInterval,
      }
      await updateAccount(
        isGraph
          ? {
              ...base,
              provider: 'graph',
              tenantId: account.tenantId,
              clientId: account.clientId,
              // App-only 账户：漏传会被后端清空 user_id，令牌从此打到 /me 而失败
              userId: account.userId,
            }
          : {
              ...base,
              imapHost: account.imapHost,
              imapPort: account.imapPort,
              imapSecurity: account.imapSecurity,
              smtpHost: account.smtpHost,
              smtpPort: account.smtpPort,
              smtpSecurity: account.smtpSecurity,
            },
      )
      toast.success(t(locale, 'settings.accountSaved'))
    } catch (error) {
      toast.error(t(locale, 'settings.saveFailed', { message: error instanceof Error ? error.message : String(error) }))
    } finally {
      setBusy(false)
    }
  }

  /** Graph 重新授权：重走浏览器授权 → 认领新 refresh_token */
  const reauthorize = async () => {
    if (reauthBusy) return
    setReauthBusy(true)
    try {
      const begin = await api.oauthBegin(account.tenantId || 'common', account.clientId, undefined, account.id)
      window.open(begin.authorizeUrl, '_blank', 'noopener')
      toast.info(t(locale, 'settings.reauthOpened'))
      const deadline = Date.now() + 5 * 60 * 1000
      let status = await api.oauthStatus(begin.state)
      while (status.status === 'pending' && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 2000))
        status = await api.oauthStatus(begin.state)
      }
      if (status.status !== 'done') {
        toast.error(
          status.status === 'failed'
            ? `${t(locale, 'wizard.authFailed')}：${status.error}`
            : t(locale, 'wizard.authExpired'),
        )
        return
      }
      await updateAccount({
        id: account.id,
        email: account.email,
        displayName: account.displayName,
        username: account.username,
        provider: 'graph',
        tenantId: account.tenantId,
        clientId: account.clientId,
        signature,
        syncEnabled: account.syncEnabled,
        syncInterval: account.syncInterval,
        oauthState: begin.state,
      })
      toast.success(t(locale, 'settings.reauthOk'))
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error))
    } finally {
      setReauthBusy(false)
    }
  }

  return (
    <div className="account-card" style={{ display: 'block' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
        <span className="addr-avatar" style={{ background: account.color }}>
          {displayName(account.displayName, account.email).slice(0, 2).toUpperCase()}
        </span>
        <div className="grow">
          <div className="acct-title">{displayName(account.displayName, account.email)}</div>
          <div className="acct-sub">
            {account.email} ·{' '}
            {isGraph
              ? `Graph OAuth2 · ${account.tenantId || 'common'}`
              : `${account.imapHost}:${account.imapPort} · ${account.smtpHost}:${account.smtpPort}`}
          </div>
          {account.lastError && (
            <div className="acct-sub" style={{ color: 'var(--red)' }}>
              {t(locale, 'settings.lastError')}: {account.lastError}
            </div>
          )}
        </div>
        {isGraph && (
          <button
            className="icon-btn"
            title={t(locale, 'settings.reauth')}
            disabled={reauthBusy}
            onClick={() => void reauthorize()}
          >
            {reauthBusy ? <Loader2 size={15} className="spin" /> : <ShieldCheck size={15} />}
          </button>
        )}
        <button className="icon-btn" title={t(locale, 'settings.syncNow')} onClick={() => void sync()}>
          <RefreshCw size={15} />
        </button>
        <button
          className="icon-btn"
          title={t(locale, 'settings.removeAccount')}
          style={{ color: confirming ? 'var(--red)' : undefined }}
          onClick={async () => {
            if (!confirming) {
              setConfirming(true)
              window.setTimeout(() => setConfirming(false), 4000)
              return
            }
            await removeAccount(account.id)
            toast.success(t(locale, 'toast.deleted'))
          }}
        >
          {confirming ? <Check size={15} /> : <Trash2 size={15} />}
        </button>
      </div>

      {confirming && (
        <div className="hint" style={{ color: 'var(--red)', marginTop: 8 }}>
          {t(locale, 'settings.removeDesc')}
        </div>
      )}

      <div className="field" style={{ marginTop: 12, marginBottom: 0 }}>
        <span className="field-label">{t(locale, 'settings.signature')}</span>
        <textarea
          className="input"
          value={signature}
          placeholder={t(locale, 'settings.signatureHint')}
          onChange={(event) => setSignature(event.target.value)}
          onBlur={() => void persist()}
        />
      </div>
      {busy && (
        <div className="hint" style={{ marginTop: 4 }}>
          <Loader2 size={11} className="spin" /> {t(locale, 'composer.saving')}
        </div>
      )}
    </div>
  )
}
