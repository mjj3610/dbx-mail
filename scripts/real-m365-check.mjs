/**
 * 真实 Microsoft 365 租户连通性检查（发版前真租户验收的自动化部分）
 *
 * 只做不依赖交互登录的网络验证；完整链路（浏览器授权 → 同步）见
 * docs/版本部署变更说明-v0.2.0.md 的真租户手动验收清单。
 *
 * 用法：
 *   node scripts/real-m365-check.mjs --tenant <tenantId> --client <clientId> [--user <email>]
 *   # 可选诊断：校验 app 是否存在于租户（区分「应用未注册」与「secret 无效」）
 *   M365_CLIENT_SECRET=<secret> node scripts/real-m365-check.mjs ...
 *
 * 注意：插件是公共客户端（PKCE），不使用 clientSecret；secret 仅用于诊断。
 * 检查项全部只读，不产生授权、不发送邮件。
 */
const args = process.argv.slice(2)
function argValue(name) {
  const index = args.indexOf(name)
  return index >= 0 ? args[index + 1] : undefined
}

const tenant = argValue('--tenant')
const clientId = argValue('--client')
const user = argValue('--user')
const clientSecret = process.env.M365_CLIENT_SECRET

if (!tenant || !clientId) {
  console.error('用法：node scripts/real-m365-check.mjs --tenant <tenantId> --client <clientId> [--user <email>]')
  process.exit(2)
}

const authority = 'https://login.microsoftonline.com'
const scopes = 'offline_access Mail.ReadWrite Mail.Send User.Read'
const failures = []
function check(name, condition, detail = '') {
  if (condition) console.log(`  ok   ${name}`)
  else {
    failures.push(`${name}${detail ? `：${detail}` : ''}`)
    console.log(`  FAIL ${name}${detail ? `：${detail}` : ''}`)
  }
}

// 与 backend/src/oauth.rs 的 authorize_url 同形（PKCE S256，占位 challenge 仅供探测）
const challenge = 'dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk'
const authorizeUrl =
  `${authority}/${tenant}/oauth2/v2.0/authorize?client_id=${encodeURIComponent(clientId)}` +
  `&response_type=code&redirect_uri=${encodeURIComponent('http://localhost')}` +
  `&response_mode=query&scope=${encodeURIComponent(scopes)}` +
  `&state=real-check&code_challenge=${challenge}&code_challenge_method=S256`

const results = {}

{
  console.log('1) OpenID 发现（租户有效性与端点）')
  try {
    const res = await fetch(`${authority}/${tenant}/v2.0/.well-known/openid-configuration`)
    const doc = await res.json()
    results.oidc = doc
    check('OIDC 配置可获取', res.ok && Boolean(doc.issuer), `HTTP ${res.status}`)
    check(
      'issuer 指向目标租户',
      typeof doc.issuer === 'string' && doc.issuer.toLowerCase().includes(tenant.toLowerCase()),
      doc.issuer,
    )
    check(
      'authorize/token 端点齐备',
      Boolean(doc.authorization_endpoint) && Boolean(doc.token_endpoint),
      [doc.authorization_endpoint, doc.token_endpoint].join(' | '),
    )
  } catch (error) {
    check('OIDC 配置可获取', false, String(error))
  }
}

{
  console.log('2) 授权端点与 Azure 应用注册')
  try {
    const res = await fetch(authorizeUrl, { redirect: 'manual' })
    const html = await res.text()
    results.authorizeStatus = res.status
    const notFound = ['AADSTS500011', 'AADSTS700016', 'AADSTS90002', 'AADSTS50020'].some((code) => html.includes(code))
    check('authorize 返回 200', res.status === 200, `HTTP ${res.status}`)
    check('应用在该租户可用（无 not-found 类 AADSTS 错误）', !notFound, notFound ? '命中 AADSTS 错误码' : '')
    // 登录页特征：微软登录页会内嵌 login 域资源与表单占位
    const looksLikeSignIn = /login\.microsoftonline|login\.msftauth\.net|Sign in|登录/.test(html)
    check('返回的是登录页而非错误页', looksLikeSignIn, html.slice(0, 120).replace(/\s+/g, ' '))
  } catch (error) {
    check('authorize 返回 200', false, String(error))
  }
}

{
  console.log('3) token 端点活性（bogus code 应得到标准错误而非应用错误）')
  try {
    const res = await fetch(`${authority}/${tenant}/oauth2/v2.0/token`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        grant_type: 'authorization_code',
        client_id: clientId,
        code: 'bogus-real-check',
        redirect_uri: 'http://localhost',
        scope: scopes,
      }),
    })
    const body = await res.json().catch(() => ({}))
    results.tokenProbe = body
    check('token 端点可达且返回 JSON', res.status >= 400 && typeof body.error === 'string', `HTTP ${res.status} ${JSON.stringify(body).slice(0, 120)}`)
    // bogus code → invalid_grant；invalid_client 才说明 client_id 有问题
    check('client_id 被端点接受（invalid_grant 而非 invalid_client）', body.error === 'invalid_grant', `${body.error} ${body.error_description || ''}`.slice(0, 200))
  } catch (error) {
    check('token 端点可达且返回 JSON', false, String(error))
  }
}

if (clientSecret) {
  console.log('4) client_credentials：换真实应用令牌（插件「纯后台接入」同款请求）')
  try {
    const res = await fetch(`${authority}/${tenant}/oauth2/v2.0/token`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        grant_type: 'client_credentials',
        client_id: clientId,
        client_secret: clientSecret,
        scope: 'https://graph.microsoft.com/.default',
      }),
    })
    const body = await res.json().catch(() => ({}))
    results.appToken = res.ok ? 'acquired' : body
    if (!res.ok) {
      const description = String(body.error_description || '')
      const verdict = description.includes('AADSTS700021')
        ? 'secret 无效或应用不允许机密客户端'
        : description.includes('AADSTS700016')
          ? '应用未在该租户注册'
          : description.slice(0, 200)
      check('应用令牌获取', false, `${body.error}：${verdict}`)
    } else {
      check('应用令牌获取', true, 'client_credentials 换取成功')
      const token = body.access_token
      if (user && token) {
        console.log(`5) 只读探测目标邮箱 /v1.0/users/${user}/mailFolders`)
        const probe = await fetch(`https://graph.microsoft.com/v1.0/users/${encodeURIComponent(user)}/mailFolders?$top=12`, {
          headers: { Authorization: `Bearer ${token}` },
        })
        const probeBody = await probe.json().catch(() => ({}))
        if (probe.ok) {
          const folders = (probeBody.value || []).map((item) => `${item.displayName}(${item.totalItemCount})`)
          check('邮箱文件夹只读探测', true, folders.join('、') || '(空)')
          const inboxId = (probeBody.value || []).find((item) => item.displayName === 'Inbox')?.id
          if (inboxId) {
            const inbox = await fetch(`https://graph.microsoft.com/v1.0/users/${encodeURIComponent(user)}/mailFolders/${encodeURIComponent(inboxId)}/messages?$top=3&$select=subject,from,receivedDateTime`, {
              headers: { Authorization: `Bearer ${token}` },
            })
            const inboxBody = await inbox.json().catch(() => ({}))
            const lines = (inboxBody.value || []).map((item) => `${(item.from?.emailAddress?.address) || '?'}: ${item.subject}`)
            check('收件箱最近 3 封只读探测', inbox.ok, inbox.ok ? lines.join(' | ') || '(空)' : JSON.stringify(inboxBody))
          }
        } else {
          const description = String(probeBody.error?.message || '')
          const verdict = probe.status === 403 || description.includes('AppOnly') || description.includes('policy')
            ? '应用令牌有效，但缺应用程序权限或未管理员同意（Azure 应用需配置 Mail.ReadWrite/Mail.Send 的「应用程序」权限并同意）'
            : probe.status === 401
              ? '令牌无效（检查应用注册）'
              : description.slice(0, 200)
          check('邮箱文件夹只读探测', false, `HTTP ${probe.status}：${verdict}`)
        }
      } else if (!user) {
        console.log('  SKIP 只读探测：未提供 --user')
      }
    }
  } catch (error) {
    check('应用令牌获取', false, String(error))
  }
} else {
  console.log('SKIP client_credentials 探测：未设置 M365_CLIENT_SECRET')
}

console.log(`\n租户：${tenant}`)
console.log(`clientId：${clientId}`)
if (user) {
  console.log(`待授权账户：${user}（需要该账户在浏览器完成交互授权后才能同步）`)
}
console.log('下一步：在插件内向导填入上述 tenant/clientId 点「授权并添加」，浏览器登录后即可真实同步。')

if (failures.length > 0) {
  console.error('\n失败项：')
  for (const item of failures) console.error(` - ${item}`)
  process.exit(1)
}
console.log('\n真实租户连通性检查全部通过')
