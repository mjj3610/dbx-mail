/**
 * DBX 宿主桥接
 *
 * 插件页面运行在沙箱工作台里，宿主注入 `window.dbxPlugin`：
 * - ready / locale / theme
 * - request: 调用 Host API（具体能力由清单 permissions 决定）
 * - invoke: 调用本插件 Sidecar 的 JSON-RPC 方法
 *
 * 纯浏览器（npm run dev）下该对象不存在，能力自动降级。
 */

export interface DbxTheme {
  appearance?: 'light' | 'dark'
  tokens?: Record<string, string>
}

export interface DbxPluginBridge {
  ready: Promise<Record<string, unknown>>
  readonly context?: Record<string, unknown>
  readonly locale: string
  readonly theme?: DbxTheme
  request(method: string, params?: unknown): Promise<unknown>
  invoke?(method: string, params?: unknown, options?: { timeoutMs?: number }): Promise<unknown>
  copy?(text: string): Promise<unknown>
  onEvent?(handler: (event: unknown) => void): () => void
  onInit?(handler: (context: Record<string, unknown>) => void): () => void
}

declare global {
  interface Window {
    dbxPlugin?: DbxPluginBridge
  }
}

/** 宿主初始化超时，避免宿主异常时白屏 */
const READY_TIMEOUT = 1500

export function bridge(): DbxPluginBridge | undefined {
  return typeof window !== 'undefined' ? window.dbxPlugin : undefined
}

export function isHosted(): boolean {
  return Boolean(bridge())
}

/** 宿主是否提供 Sidecar 通道 */
export function hasSidecar(): boolean {
  return typeof bridge()?.invoke === 'function'
}

/**
 * 判断宿主外观：宿主切换深浅色时只更新 html[data-dbx-theme]，
 * 注入时的 theme 值可能是旧的。
 */
export function hostAppearance(): 'light' | 'dark' {
  const fromDom = document.documentElement.dataset.dbxTheme
  const source = fromDom || bridge()?.theme?.appearance || 'light'
  return source === 'dark' ? 'dark' : 'light'
}

/** 宿主语言归一化为 zh-CN / en */
export function hostLocale(): string {
  const raw = (bridge()?.locale ?? 'zh-CN').toLowerCase()
  return raw.startsWith('zh') ? 'zh-CN' : 'en'
}

/**
 * 宿主工作台下发的连接上下文：connection-provider 打开的 tab 携带 connectionId，
 * 用于「一个连接一个邮箱」的作用域模式；独立打开（插件中心等）返回空串。
 */
export function hostConnectionId(): string {
  const value = bridge()?.context?.connectionId
  return typeof value === 'string' ? value : ''
}

/**
 * 订阅宿主 init：context 经 postMessage init 下发，可能晚于插件启动
 * （宿主等 iframe ready 后才发 init）。返回退订函数。
 */
export function onHostInit(handler: () => void): () => void {
  const off = bridge()?.onInit?.(() => handler())
  return off ?? (() => {})
}

/** 监听宿主外观变化 */
export function watchHostAppearance(onChange: () => void): () => void {
  if (typeof MutationObserver === 'undefined') return () => {}
  const observer = new MutationObserver(() => onChange())
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['data-dbx-theme'],
  })
  return () => observer.disconnect()
}

/** 等待宿主就绪（失败也不阻塞，按独立模式继续） */
export async function awaitHostReady(): Promise<void> {
  const host = bridge()
  if (!host?.ready) return
  await Promise.race([
    host.ready.catch(() => undefined),
    new Promise((resolve) => setTimeout(resolve, READY_TIMEOUT)),
  ])
}

/**
 * 写系统剪贴板
 *
 * 本插件清单不声明任何 Host API 权限（宿主只接受 `host.network:<url>` 这类网络权限，
 * 通用 Host API 权限会被判定为不兼容），因此按「宿主能力 → 浏览器 API → 兜底」逐级降级。
 * @returns 是否成功写入
 */
export async function copyText(text: string): Promise<boolean> {
  const host = bridge()
  if (host?.copy) {
    try {
      await host.copy(text)
      return true
    } catch {
      /* 宿主未授予该能力，继续降级 */
    }
  }
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    /* 沙箱里 navigator.clipboard 可能被禁用 */
  }
  try {
    const area = document.createElement('textarea')
    area.value = text
    area.style.position = 'fixed'
    area.style.top = '-1000px'
    area.style.opacity = '0'
    document.body.appendChild(area)
    area.select()
    const ok = document.execCommand('copy')
    area.remove()
    return ok
  } catch {
    return false
  }
}
