/**
 * 渲染验收脚本：打开 demo 界面 → 打开富格式样例邮件 → 拦截态截图 → 放开图片 → 再截图
 * 顺带校验 <style> 作用域、图片尺寸与 class，避免只看截图靠肉眼猜。
 */
import { fileURLToPath } from 'node:url'
import { chromium, executablePath } from './browser.mjs'
import { mkdirSync } from 'node:fs'

const out = fileURLToPath(new URL('../screenshots/', import.meta.url))
const exe = executablePath
mkdirSync(out, { recursive: true })

const browser = await chromium.launch({ executablePath: exe })
const page = await browser.newPage({ viewport: { width: 1180, height: 1000 } })
await page.goto('http://127.0.0.1:4173/index.html?demo=1&rich=1', { waitUntil: 'networkidle' })
await page.getByText('Validate catalog workflow run').first().click()
await page.waitForTimeout(900)
await page.screenshot({ path: `${out}/rich-blocked.png` })

const blocked = await page.evaluate(() =>
  Array.from(document.querySelectorAll('.mail-html img')).map((img) => ({
    alt: img.alt,
    cls: img.className,
    tracking: img.dataset.tracking ?? '',
    w: Math.round(img.getBoundingClientRect().width),
    h: Math.round(img.getBoundingClientRect().height),
    display: getComputedStyle(img).display,
  })),
)
console.log('拦截态图片:', JSON.stringify(blocked))

const scope = await page.evaluate(() => {
  const mail = document.querySelector('.mail-html')
  const heading = document.querySelector('.mail-html h3')
  const list = document.querySelector('.mail-html ul')
  return {
    mailBg: mail ? getComputedStyle(mail).backgroundColor : 'none',
    bodyBg: getComputedStyle(document.body).backgroundColor,
    h3: heading ? { color: getComputedStyle(heading).color, spacing: getComputedStyle(heading).letterSpacing } : null,
    listStyle: list ? getComputedStyle(list).listStyleType : 'none',
    scopedStyles: Array.from(document.querySelectorAll('style'))
      .map((node) => node.textContent ?? '')
      .filter((text) => text.includes('.mail-html'))
      .map((text) => text.replace(/\s+/g, ' ').slice(0, 120)),
  }
})
console.log('作用域校验:', JSON.stringify(scope, null, 1))

const button = page.getByRole('button', { name: '显示图片' })
if (await button.count()) await button.click()
await page.waitForTimeout(1200)
await page.screenshot({ path: `${out}/rich-loaded.png` })

const after = await page.evaluate(() =>
  Array.from(document.querySelectorAll('.mail-html img')).map((img) => ({
    cls: img.className,
    display: getComputedStyle(img).display,
    w: Math.round(img.getBoundingClientRect().width),
    h: Math.round(img.getBoundingClientRect().height),
  })),
)
console.log('放开态图片:', JSON.stringify(after))

const errors = []
page.on('console', (message) => {
  if (message.type() === 'error') errors.push(message.text())
})
console.log('控制台错误:', errors.length ? errors : '无')
await browser.close()
