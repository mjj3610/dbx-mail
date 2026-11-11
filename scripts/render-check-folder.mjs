/**
 * 文件夹同步检查：
 * 1) 同角色的两个文件夹（Junk / 广告邮件）在侧栏用真实名字区分，不重复成两个「垃圾邮件」
 * 2) 打开从未同步过的自定义文件夹（订阅邮件）时自动触发一次同步，并给出「同步此文件夹」入口
 */
import { fileURLToPath } from 'node:url'
import { chromium, executablePath } from './browser.mjs'

const exe = executablePath
const out = fileURLToPath(new URL('../screenshots/', import.meta.url))
const browser = await chromium.launch({ executablePath: exe })
const page = await browser.newPage({ viewport: { width: 1180, height: 900 } })
const errors = []
page.on('console', (message) => {
  if (message.type() === 'error') errors.push(message.text())
})
await page.goto('http://127.0.0.1:4173/index.html?demo=1', { waitUntil: 'networkidle' })

const labels = await page.evaluate(() =>
  Array.from(document.querySelectorAll('.nav-row .nav-text'))
    .map((node) => node.textContent?.trim() ?? '')
    .filter(Boolean),
)
console.log('侧栏文件夹:', JSON.stringify(labels))

await page.getByRole('button', { name: '订阅邮件' }).click()
await page.waitForTimeout(150)
const during = await page.evaluate(() => {
  const button = Array.from(document.querySelectorAll('.empty-list button')).at(-1)
  return {
    emptyTitle: document.querySelector('.empty-list .title')?.textContent?.trim() ?? '',
    hasSyncButton: Boolean(button),
    buttonLabel: button?.textContent?.trim() ?? '',
    buttonDisabled: button instanceof HTMLButtonElement ? button.disabled : null,
    headerSyncing: Boolean(document.querySelector('.cap-btn .spin')),
  }
})
console.log('打开文件夹瞬间:', JSON.stringify(during, null, 1))
await page.screenshot({ path: `${out}/folder-sync.png` })

await page.waitForTimeout(1400)
const after = await page.evaluate(() => {
  const button = Array.from(document.querySelectorAll('.empty-list button')).at(-1)
  return {
    buttonDisabled: button instanceof HTMLButtonElement ? button.disabled : null,
    headerSyncing: Boolean(document.querySelector('.cap-btn .spin')),
  }
})
console.log('同步结束:', JSON.stringify(after))
console.log('控制台错误:', errors.length ? errors : '无')
await browser.close()
