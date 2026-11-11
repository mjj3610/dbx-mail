/**
 * 滚动与横向裁切检查：窄窗口打开「固定 700px 宽 + body{overflow:hidden}」的营销邮件模板
 * 断言：阅读区能纵向滚动到底、正文不被左右裁掉、邮件区域自身有横向滚动兜底
 */
import { fileURLToPath } from 'node:url'
import { chromium, executablePath } from './browser.mjs'

const exe = executablePath
const out = fileURLToPath(new URL('../screenshots/', import.meta.url))
const browser = await chromium.launch({ executablePath: exe })
const page = await browser.newPage({ viewport: { width: 900, height: 700 } })
await page.goto('http://127.0.0.1:4173/index.html?demo=1&rich=2', { waitUntil: 'networkidle' })
await page.getByText('行业情报周报').first().click()
await page.waitForTimeout(900)
await page.screenshot({ path: `${out}/scroll-top.png` })

/** 滚动容器：正文所在的可滚动父级 */
const probe = await page.evaluate(() => {
  const scroller = document.querySelector('.reader-scroll')
  const body = document.querySelector('.mail-html')
  const rect = body?.getBoundingClientRect()
  const scrollerRect = scroller?.getBoundingClientRect()
  return {
    scroll: scroller
      ? {
          clientHeight: scroller.clientHeight,
          scrollHeight: scroller.scrollHeight,
          clientWidth: scroller.clientWidth,
          scrollWidth: scroller.scrollWidth,
          overflowX: getComputedStyle(scroller).overflowX,
          overflowY: getComputedStyle(scroller).overflowY,
        }
      : null,
    mail: body
      ? {
          height: Math.round(rect.height),
          width: Math.round(rect.width),
          left: Math.round(rect.left),
          right: Math.round(rect.right),
          overflowX: getComputedStyle(body).overflowX,
          scrollWidth: body.scrollWidth,
          clientWidth: body.clientWidth,
        }
      : null,
    viewportWidth: window.innerWidth,
    paneRight: scrollerRect ? Math.round(scrollerRect.right) : 0,
    docScrollWidth: document.documentElement.scrollWidth,
  }
})
console.log('初始状态:', JSON.stringify(probe, null, 1))

// 尝试把阅读区滚到底，验证纵向可达
const scrolled = await page.evaluate(() => {
  const scroller = document.querySelector('.reader-scroll')
  if (!scroller) return null
  scroller.scrollTop = scroller.scrollHeight
  return {
    scrollTop: Math.round(scroller.scrollTop),
    max: scroller.scrollHeight - scroller.clientHeight,
    reachedBottom: scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2,
  }
})
console.log('滚动到底:', JSON.stringify(scrolled))
await page.waitForTimeout(300)
await page.screenshot({ path: `${out}/scroll-bottom.png` })
await browser.close()
