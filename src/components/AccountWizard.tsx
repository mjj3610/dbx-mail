import { useEffect, useMemo, useRef, useState } from 'react'
import { CheckCircle2, ChevronDown, ExternalLink, Loader2, Plus, Server, X } from 'lucide-react'
import { toast } from 'sonner'
import { t } from '../lib/i18n'
import * as api from '../lib/rpc'
import type { ProviderPreset } from '../lib/rpc'
import { useMail } from '../stores/mail'
import { useUI } from '../stores/ui'

interface FormState {
  email: string
  password: string
  displayName: string
  imapHost: string
  imapPort: number
  imapSecurity: string
  smtpHost: string
  smtpPort: number
  smtpSecurity: string
  signature: string
  tenantId: string
  clientId: string
  graphMode: 'browser' | 'app'
  clientSecret: string
  userId: string
}

const EMPTY: FormState = {
  email: '',
  password: '',
  displayName: '',
  imapHost: '',
  imapPort: 993,
  imapSecurity: 'tls',
  smtpHost: '',
  smtpPort: 465,
  smtpSecurity: 'tls',
  signature: '',
  tenantId: 'common',
  clientId: '',
  graphMode: 'browser',
  clientSecret: '',
  userId: '',
}

type ProviderChoice = 'auto' | 'imap' | 'graph'
type AuthPhase = 'idle' | 'auth' | 'done' | 'failed'

export function AccountWizard() {
  const locale = useUI((state) => state.locale)
  const closeDialog = useUI((state) => state.closeDialog)
  const addAccount = useMail((state) => state.addAccount)
  const [form, setForm] = useState<FormState>(EMPTY)
  const [presets, setPresets] = useState<ProviderPreset[]>([])
  const [providerChoice, setProviderChoice] = useState<ProviderChoice>('auto')
  const [advanced, setAdvanced] = useState(false)
  const [testing, setTesting] = useState(false)
  const [busy, setBusy] = useState(false)
  const [lines, setLines] = useState<string[]>([])
  const [error, setError] = useState('')
  const [authPhase, setAuthPhase] = useState<AuthPhase>('idle')
  const [authUrl, setAuthUrl] = useState('')
  const authStateRef = useRef('')
  const cancelledRef = useRef(false)

  useEffect(() => {
    void api
      .accountPresets()
      .then((result) => setPresets(result.items))
      .catch(() => setPresets([]))
    return () => {
      // 离开向导时取消还在等待的授权回调
      cancelledRef.current = true
      if (authStateRef.current) void api.oauthCancel(authStateRef.current)
    }
  }, [])

  const domain = form.email.split('@')[1]?.toLowerCase() ?? ''
  const matched = useMemo(
    () =>
      presets.find((preset) =>
        preset.domains.some((item) => domain === item || domain.endsWith(`.${item}`)),
      ),
    [presets, domain],
  )
  const isGraph =
    providerChoice === 'graph' || (providerChoice === 'auto' && matched?.provider === 'graph')

  useEffect(() => {
    if (!matched || providerChoice !== 'auto' || matched.provider === 'graph') return
    setForm((state) => ({
      ...state,
      imapHost: matched.imapHost,
      imapPort: matched.imapPort,
      imapSecurity: matched.imapSecurity,
      smtpHost: matched.smtpHost,
      smtpPort: matched.smtpPort,
      smtpSecurity: matched.smtpSecurity,
    }))
  }, [matched, providerChoice])

  const update = (patch: Partial<FormState>) => setForm((state) => ({ ...state, ...patch }))

  const basePayload = () => ({
    email: form.email.trim(),
    displayName: form.displayName.trim(),
    username: form.email.trim(),
    signature: form.signature,
    verify: false,
  })

  const imapPayload = () => ({
    ...basePayload(),
    password: form.password,
    imapHost: form.imapHost.trim(),
    imapPort: form.imapPort,
    imapSecurity: form.imapSecurity,
    smtpHost: form.smtpHost.trim(),
    smtpPort: form.smtpPort,
    smtpSecurity: form.smtpSecurity,
  })

  const graphPayload = (oauthState: string) => ({
    ...basePayload(),
    provider: 'graph',
    tenantId: form.tenantId.trim() || 'common',
    clientId: form.clientId.trim(),
    ...(form.clientSecret.trim() ? { clientSecret: form.clientSecret.trim() } : {}),
    oauthState,
  })

  /** 纯后台接入（client_credentials）：无浏览器交互，直接访问 userId 邮箱 */
  const graphAppPayload = () => ({
    ...basePayload(),
    provider: 'graph',
    tenantId: form.tenantId.trim() || 'common',
    clientId: form.clientId.trim(),
    clientSecret: form.clientSecret.trim(),
    userId: (form.userId.trim() || form.email.trim()),
    verify: true,
  })

  const test = async () => {
    if (!form.email.includes('@') || !form.password) {
      setError(t(locale, 'wizard.required'))
      return
    }
    setTesting(true)
    setError('')
    setLines([])
    try {
      const result = await api.accountTest({ ...imapPayload(), verify: true })
      setLines([t(locale, 'wizard.testOk'), ...result.lines])
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setTesting(false)
    }
  }

  /** Graph 授权：起回调监听 → 打开授权页 → 轮询 → 认领 token 添加账户 */
  const authorizeAndAdd = async () => {
    if (!form.email.includes('@')) {
      setError(t(locale, 'wizard.required'))
      return
    }
    if (!form.clientId.trim()) {
      setError(t(locale, 'wizard.clientIdRequired'))
      return
    }
    setBusy(true)
    setError('')
    setAuthPhase('auth')
    try {
      const begin = await api.oauthBegin(form.tenantId.trim() || 'common', form.clientId.trim(), form.clientSecret.trim() || undefined)
      authStateRef.current = begin.state
      setAuthUrl(begin.authorizeUrl)
      window.open(begin.authorizeUrl, '_blank', 'noopener')
      // 轮询授权结果（最长 5 分钟，与 sidecar 回调超时一致）
      const deadline = Date.now() + 5 * 60 * 1000
      let status = await api.oauthStatus(begin.state)
      while (status.status === 'pending' && Date.now() < deadline && !cancelledRef.current) {
        await new Promise((resolve) => setTimeout(resolve, 2000))
        status = await api.oauthStatus(begin.state)
      }
      if (status.status !== 'done') {
        setAuthPhase('failed')
        setError(
          status.status === 'failed'
            ? `${t(locale, 'wizard.authFailed')}：${status.error}`
            : t(locale, 'wizard.authExpired'),
        )
        return
      }
      await addAccount(graphPayload(begin.state))
      setAuthPhase('done')
      toast.success(t(locale, 'wizard.added'))
      closeDialog()
    } catch (cause) {
      setAuthPhase('failed')
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const submit = async () => {
    if (isGraph) {
      if (form.graphMode === 'app') {
        // 纯后台接入：无浏览器交互，直接创建并验证
        if (!form.clientSecret.trim()) {
          setError(t(locale, 'wizard.clientSecretRequired'))
          return
        }
        setBusy(true)
        setError('')
        try {
          await addAccount(graphAppPayload())
          toast.success(t(locale, 'wizard.added'))
          closeDialog()
        } catch (cause) {
          setError(cause instanceof Error ? cause.message : String(cause))
        } finally {
          setBusy(false)
        }
        return
      }
      await authorizeAndAdd()
      return
    }
    if (!form.email.includes('@') || !form.password) {
      setError(t(locale, 'wizard.required'))
      return
    }
    setBusy(true)
    setError('')
    try {
      await addAccount(imapPayload())
      toast.success(t(locale, 'wizard.added'))
      closeDialog()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const copyAuthUrl = async () => {
    if (!authUrl) return
    try {
      await navigator.clipboard.writeText(authUrl)
      toast.success(t(locale, 'action.copied'))
    } catch {
      toast.error(t(locale, 'action.copyFailed'))
    }
  }

  return (
    <div className="overlay" style={{ zIndex: 45 }}>
      <div className="dialog md">
        <div className="dialog-head">
          <Plus size={16} style={{ color: 'var(--accent)' }} />
          <span className="dialog-title">{t(locale, 'wizard.title')}</span>
          <button className="icon-btn" onClick={closeDialog}>
            <X size={16} />
          </button>
        </div>

        <div className="dialog-body">
          <div className="field">
            <span className="field-label">{t(locale, 'wizard.email')}</span>
            <input
              className="input"
              autoFocus
              value={form.email}
              placeholder="name@example.com"
              onChange={(event) => update({ email: event.target.value })}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && !isGraph) void submit()
              }}
            />
          </div>

          {isGraph ? (
            <>
              <div className="field">
                <span className="field-label">{t(locale, 'wizard.tenant')}</span>
                <input
                  className="input"
                  value={form.tenantId}
                  placeholder="common"
                  onChange={(event) => update({ tenantId: event.target.value })}
                />
                <span className="hint">{t(locale, 'wizard.tenantHint')}</span>
              </div>
              <div className="field">
                <span className="field-label">{t(locale, 'wizard.clientId')}</span>
                <input
                  className="input"
                  value={form.clientId}
                  placeholder="00000000-0000-0000-0000-000000000000"
                  onChange={(event) => update({ clientId: event.target.value })}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') void submit()
                  }}
                />
                <span className="hint">{t(locale, 'wizard.clientIdHint')}</span>
              </div>
              <div className="field">
                <span className="field-label">{t(locale, 'wizard.graphMode')}</span>
                <select
                  className="select"
                  value={form.graphMode}
                  onChange={(event) => update({ graphMode: event.target.value as 'browser' | 'app' })}
                >
                  <option value="browser">{t(locale, 'wizard.graphMode.browser')}</option>
                  <option value="app">{t(locale, 'wizard.graphMode.app')}</option>
                </select>
              </div>
              <div className="field">
                <span className="field-label">
                  {t(locale, 'wizard.clientSecret')}
                  {form.graphMode === 'app' ? ' *' : ''}
                </span>
                <input
                  className="input"
                  type="password"
                  value={form.clientSecret}
                  onChange={(event) => update({ clientSecret: event.target.value })}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') void submit()
                  }}
                />
                <span className="hint">
                  {t(locale, form.graphMode === 'app' ? 'wizard.clientSecretHint.app' : 'wizard.clientSecretHint.browser')}
                </span>
              </div>
              {form.graphMode === 'app' && (
                <div className="field">
                  <span className="field-label">{t(locale, 'wizard.userId')}</span>
                  <input
                    className="input"
                    value={form.userId}
                    placeholder={form.email}
                    onChange={(event) => update({ userId: event.target.value })}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter') void submit()
                    }}
                  />
                  <span className="hint">{t(locale, 'wizard.userIdHint')}</span>
                </div>
              )}
              <div
                style={{
                  display: 'flex',
                  gap: 8,
                  padding: '9px 11px',
                  borderRadius: 10,
                  background: 'var(--accent-tint)',
                  color: 'var(--accent)',
                  fontSize: 12.5,
                  marginBottom: 12,
                  lineHeight: 1.55,
                }}
              >
                <CheckCircle2 size={14} style={{ marginTop: 1, flex: 'none' }} />
                <span>{t(locale, form.graphMode === 'app' ? 'wizard.appGuide' : 'wizard.azureGuide')}</span>
              </div>
              {authPhase === 'auth' && authUrl && (
                <div className="field">
                  <span className="field-label">{t(locale, 'wizard.authWaiting')}</span>
                  <div className="field-row">
                    <button className="btn btn-ghost btn-sm" onClick={() => void copyAuthUrl()}>
                      {t(locale, 'action.copyLink')}
                    </button>
                    <button className="btn btn-ghost btn-sm" onClick={() => window.open(authUrl, '_blank', 'noopener')}>
                      <ExternalLink size={13} />
                      {t(locale, 'action.openLink')}
                    </button>
                  </div>
                  <span className="hint" style={{ overflowWrap: 'anywhere' }}>
                    {authUrl}
                  </span>
                </div>
              )}
            </>
          ) : (
            <>
              <div className="field">
                <span className="field-label">{t(locale, 'wizard.password')}</span>
                <input
                  className="input"
                  type="password"
                  value={form.password}
                  onChange={(event) => update({ password: event.target.value })}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') void submit()
                  }}
                />
                <span className="hint">{t(locale, 'wizard.passwordHint')}</span>
              </div>
            </>
          )}

          <div className="field">
            <span className="field-label">{t(locale, 'wizard.displayName')}</span>
            <input
              className="input"
              value={form.displayName}
              onChange={(event) => update({ displayName: event.target.value })}
            />
          </div>

          {matched && providerChoice === 'auto' && (
            <div
              style={{
                display: 'flex',
                gap: 8,
                padding: '9px 11px',
                borderRadius: 10,
                background: 'var(--accent-tint)',
                color: 'var(--accent)',
                fontSize: 12.5,
                marginBottom: 12,
              }}
            >
              <CheckCircle2 size={14} style={{ marginTop: 1, flex: 'none' }} />
              <span>
                <b>{t(locale, 'wizard.autoConfig')}</b> · {matched.label}
                {matched.hint ? ` · ${matched.hint}` : ''}
              </span>
            </div>
          )}

          <button
            className="btn btn-ghost btn-sm"
            style={{ marginBottom: advanced ? 10 : 4 }}
            onClick={() => setAdvanced(!advanced)}
          >
            <Server size={13} />
            {advanced ? t(locale, 'action.hideDetails') : t(locale, 'wizard.advanced')}
            <ChevronDown size={13} style={{ transform: advanced ? 'rotate(180deg)' : 'none' }} />
          </button>

          {advanced && (
            <div className="fade-in">
              <div className="field">
                <span className="field-label">{t(locale, 'wizard.provider')}</span>
                <select
                  className="select"
                  value={providerChoice}
                  onChange={(event) => {
                    setProviderChoice(event.target.value as ProviderChoice)
                    setAuthPhase('idle')
                    setError('')
                  }}
                >
                  <option value="auto">{t(locale, 'wizard.provider.auto')}</option>
                  <option value="imap">{t(locale, 'wizard.provider.imap')}</option>
                  <option value="graph">{t(locale, 'wizard.provider.graph')}</option>
                </select>
                <span className="hint">{t(locale, 'wizard.providerHint')}</span>
              </div>

              {!isGraph && (
                <>
                  <div className="field-label" style={{ marginBottom: 6 }}>
                    {t(locale, 'wizard.imap')}
                  </div>
                  <div className="field-row" style={{ marginBottom: 12 }}>
                    <input
                      className="input"
                      placeholder="imap.example.com"
                      value={form.imapHost}
                      onChange={(event) => update({ imapHost: event.target.value })}
                    />
                    <input
                      className="input"
                      type="number"
                      value={form.imapPort}
                      onChange={(event) => update({ imapPort: Number(event.target.value) || 993 })}
                    />
                    <select
                      className="select"
                      value={form.imapSecurity}
                      onChange={(event) => update({ imapSecurity: event.target.value })}
                    >
                      <option value="tls">{t(locale, 'wizard.security.tls')}</option>
                      <option value="starttls">{t(locale, 'wizard.security.starttls')}</option>
                      <option value="none">{t(locale, 'wizard.security.none')}</option>
                    </select>
                  </div>

                  <div className="field-label" style={{ marginBottom: 6 }}>
                    {t(locale, 'wizard.smtp')}
                  </div>
                  <div className="field-row" style={{ marginBottom: 12 }}>
                    <input
                      className="input"
                      placeholder="smtp.example.com"
                      value={form.smtpHost}
                      onChange={(event) => update({ smtpHost: event.target.value })}
                    />
                    <input
                      className="input"
                      type="number"
                      value={form.smtpPort}
                      onChange={(event) => update({ smtpPort: Number(event.target.value) || 465 })}
                    />
                    <select
                      className="select"
                      value={form.smtpSecurity}
                      onChange={(event) => update({ smtpSecurity: event.target.value })}
                    >
                      <option value="tls">{t(locale, 'wizard.security.tls')}</option>
                      <option value="starttls">{t(locale, 'wizard.security.starttls')}</option>
                      <option value="none">{t(locale, 'wizard.security.none')}</option>
                    </select>
                  </div>
                </>
              )}

              <div className="field">
                <span className="field-label">{t(locale, 'settings.signature')}</span>
                <textarea
                  className="input"
                  value={form.signature}
                  onChange={(event) => update({ signature: event.target.value })}
                />
              </div>
            </div>
          )}

          {error && (
            <div style={{ color: 'var(--red)', fontSize: 12.5, lineHeight: 1.5, marginTop: 4 }}>{error}</div>
          )}
          {lines.length > 0 && (
            <div style={{ marginTop: 8, fontSize: 12.5, color: 'var(--green)' }}>
              {lines.map((line) => (
                <div key={line}>· {line}</div>
              ))}
            </div>
          )}
        </div>

        <div className="dialog-foot">
          {!isGraph && (
            <button className="btn btn-ghost" onClick={() => void test()} disabled={testing}>
              {testing ? <Loader2 size={13} className="spin" /> : <Server size={13} />}
              {testing ? t(locale, 'wizard.testing') : t(locale, 'wizard.test')}
            </button>
          )}
          <span className="grow" />
          <button className="btn" onClick={closeDialog}>
            {t(locale, 'action.cancel')}
          </button>
          <button className="btn btn-primary" onClick={() => void submit()} disabled={busy || authPhase === 'auth'}>
            {busy ? <Loader2 size={13} className="spin" /> : <Plus size={13} />}
            {isGraph
              ? busy
                ? t(locale, 'wizard.authorizing')
                : t(locale, 'wizard.authAdd')
              : busy
                ? t(locale, 'wizard.adding')
                : t(locale, 'wizard.add')}
          </button>
        </div>
      </div>
    </div>
  )
}
