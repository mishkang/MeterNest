const { createHash } = require('node:crypto')
const { checkin, STATUS_PATH, CLAIM_PATH } = require('../automations/workbuddy-checkin')
// Runs only in the installed WorkBuddy runtime's Node mode, never its app main.
// Keys/tokens stay inside this short-lived process; stdout returns display data.
const fs = require('node:fs/promises')
const { constants } = require('node:fs')
const path = require('node:path')
const { normalizeSummary, SUMMARY_PATH, PAID_PATH, FREE_PATH, fetchCreditData } = require('./workbuddy-data')
const { normalizeModels } = require('./workbuddy-models')
const { attachClientIcons } = require('./workbuddy-icons')

const ORIGINS = new Set(['https://www.workbuddy.cn', 'https://www.workbuddy.ai', 'https://www.codebuddy.ai',
  'https://wb.tencentbuddy.com', 'https://copilot.tencent.com'])
const MESSAGES = {
  AUTH_REQUIRED: '请先在 WorkBuddy 中登录一次，再刷新积分。',
  AUTH_EXPIRED: 'WorkBuddy 登录凭据已过期，请打开 WorkBuddy 重新登录后再查询。未自动刷新或修改登录状态。',
  UNSUPPORTED_RUNTIME: '当前 WorkBuddy 版本暂不支持退出后的积分查询。',
  UNSUPPORTED_ACCOUNT: '暂不支持此 WorkBuddy 企业账号的独立积分查询。',
  NETWORK_ERROR: 'WorkBuddy 积分查询失败，请检查网络后重试。',
  SESSION_CHANGED: 'WorkBuddy 正在更新本机登录凭据，将自动重新查询。',
  QUERY_TIMEOUT: 'WorkBuddy 积分查询超时，请检查网络后重试。'
}
function failure(code) { return Object.assign(new Error(MESSAGES[code]), { code }) }
async function readJson(file, io, maxBytes = 256 * 1024) {
  const handle = await io.open(file, constants.O_RDONLY | (constants.O_NOFOLLOW || 0))
  try {
    const stat = await handle.stat()
    if (!stat.isFile() || stat.size > maxBytes || (process.getuid && (stat.uid !== process.getuid() || stat.mode & 0o022))) throw failure('AUTH_REQUIRED')
    const body = await handle.readFile('utf8')
    if (Buffer.byteLength(body) > maxBytes) throw failure('AUTH_REQUIRED')
    return JSON.parse(body)
  } finally { await handle.close() }
}
async function loggedOut(file, io) {
  try { await io.lstat(`${file}.logged-out`); return true } catch (error) { if (error.code === 'ENOENT') return false; throw error }
}
async function readProductConfig(configDir, io) {
  const directory = path.join(configDir, 'cache')
  try { return await readJson(path.join(directory, 'acc-product-config-v3.json'), io, 2 * 1024 * 1024) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  // Since 5.9.3 each desktop process writes a private, uniquely named cache.
  // Only inspect exact cache names; never execute product configuration code.
  const names = (await io.readdir(directory)).filter(name => /^acc-product-config-v3-\d+-[0-9a-f-]+\.json$/.test(name))
  if (!names.length || names.length > 200) throw failure('UNSUPPORTED_RUNTIME')
  const files = await Promise.all(names.map(async name => {
    const file = path.join(directory, name)
    const stat = await io.lstat(file)
    return { file, time: stat.mtimeMs, regular: stat.isFile() }
  }))
  const latest = files.filter(file => file.regular).sort((a, b) => b.time - a.time)[0]
  if (!latest) throw failure('UNSUPPORTED_RUNTIME')
  return readJson(latest.file, io, 2 * 1024 * 1024)
}
function storageLoggerGet() {
  try { return process._linkedBinding('electron_browser_workbuddy_storage').loggerGet() }
  catch { return require('electron').workbuddyStorage.loggerGet() }
}
function loadLegacyCrypto(root, load = require) {
  try {
    // WorkBuddy 5.9 moved crypto out of app-lifetime-job into dist.js. Only
    // initialize the crypto module; never load the desktop entry or migrations.
    const legacy = load(path.join(root, 'main', 'app-lifetime-job.js'))
    if ([legacy.D, legacy.G, legacy.K].every(value => typeof value === 'function')) {
      legacy.D()
      if (typeof legacy.R !== 'function') return null
      return { normalize: legacy.G, Crypto: legacy.R, parseEnvelope: legacy.K }
    }
    const current = load(path.join(root, 'main', 'dist.js'))
    if (![current.s, current.T, current.E].every(value => typeof value === 'function')) return null
    current.s()
    if (typeof current.v !== 'function') return null
    return { normalize: current.T, Crypto: current.v, parseEnvelope: current.E }
  } catch { return null }
}
function loadModernCrypto(root, configDir, load = require) {
  try {
    // WorkBuddy 5.11 moved the at-rest implementation from dist.js to src.js
    // and exposes it through credential-protection.js.
    const src = load(path.join(root, 'main', 'src.js'))
    const protection = load(path.join(root, 'main', 'credential-protection.js'))
    if (![src._, src.t, src.f, src.d].every(value => typeof value === 'function')) return null
    if (![protection._, protection.t, protection.v, protection.b].every(value => typeof value === 'function')) return null
    src.t()
    protection.t()
    const capabilities = src._(storageLoggerGet())
    if (capabilities?.symmetric?.status !== 'available') return null
    const bootstrap = { policy: 'fields', symmetricKey: capabilities.symmetric.value }
    if (capabilities.developerPublicKey?.status === 'available') bootstrap.developerPublicKey = capabilities.developerPublicKey.value
    try {
      const keyblob = src.f({ keyblobPath: src.d(configDir), protectorKey: capabilities.symmetric.value, createIfMissing: false })
      if (keyblob?.status === 'available') bootstrap.userKey = keyblob.value
    } catch { /* Standard field encryption only needs the symmetric key. */ }
    try { protection._(bootstrap) }
    finally {
      capabilities.symmetric.value.key.fill(0)
      bootstrap.userKey?.key.fill(0)
    }
    const fieldCodec = protection.v({ purpose: 'workbuddy-user-settings', resourceId: 'settings/user', scope: 'field' })
    return {
      modern: true,
      fieldCodec,
      dispose() { fieldCodec.dispose(); protection.b() }
    }
  } catch { return null }
}
function runtimeExtras(root) {
  try {
    const { fetch, Agent } = require(path.join(root, 'node_modules', 'undici'))
    let userAgent = ''
    try {
      const version = require(path.join(root, 'package.json')).version
      const cliVersion = require(path.join(root, 'cli', 'package.json')).version
      if (/^\d+\.\d+\.\d+[\w.-]*$/.test(version) && /^\d+\.\d+\.\d+[\w.-]*$/.test(cliVersion)) userAgent = `WorkBuddy/${version} CLI/${cliVersion}`
    } catch { /* Older runtimes can still provide the existing credit query. */ }
    return { fetch, Agent, userAgent }
  } catch { return null }
}
function getRuntime(root, configDir) {
  try {
    const extras = runtimeExtras(root)
    if (!extras) throw failure('UNSUPPORTED_RUNTIME')
    const core = loadLegacyCrypto(root)
    if (core) {
      // Native getter only. Never call loggerIn/loggerOut or create a keyblob.
      const keys = core.normalize(storageLoggerGet())
      if (keys.symmetric.status !== 'available') throw failure('UNSUPPORTED_RUNTIME')
      let codec
      try { codec = new core.Crypto(keys.symmetric.value) } finally { keys.symmetric.value.key.fill(0) }
      return { codec, parseEnvelope: core.parseEnvelope, ...extras }
    }
    const modern = loadModernCrypto(root, configDir)
    if (modern) return { ...modern, ...extras }
    throw failure('UNSUPPORTED_RUNTIME')
  } catch { throw failure('UNSUPPORTED_RUNTIME') }
}
function decodeField(value, runtime, fieldPath = '/value') {
  if (typeof value === 'string') return value
  if (value?.$wbEncrypted !== 1 || typeof value.envelope !== 'string' || value.envelope.length > 65536) throw failure('UNSUPPORTED_RUNTIME')
  if (runtime.fieldCodec) {
    const decoded = runtime.fieldCodec.decode({ value }, [{ path: ['value'], fieldPath }])
    if (typeof decoded?.value?.value !== 'string') throw failure('UNSUPPORTED_RUNTIME')
    return decoded.value.value
  }
  if (value.scheme) throw failure('UNSUPPORTED_RUNTIME')
  const encrypted = Buffer.from(value.envelope, 'base64')
  if (encrypted.toString('base64') !== value.envelope) throw failure('UNSUPPORTED_RUNTIME')
  const plain = runtime.codec.open(runtime.parseEnvelope(encrypted), { framing: 'field' })
  try { return plain.toString('utf8') } finally { plain.fill(0) }
}
async function query({ root, sharedDir, configDir, kind = 'credits', expectedAccountKey }, { io = fs, loadRuntime = getRuntime, now = Date.now } = {}) {
  if (!['credits', 'summary', 'models', 'identity', 'checkin', 'checkin-status'].includes(kind)) throw failure('UNSUPPORTED_RUNTIME')
  const authPath = path.join(sharedDir, 'auth', 'workbuddy-desktop.info')
  let session
  try {
    if (await loggedOut(authPath, io)) throw failure('AUTH_REQUIRED')
    session = await readJson(authPath, io)
  } catch { throw failure('AUTH_REQUIRED') }
  if (!session?.account?.uid || !session.auth?.accessToken) throw failure('AUTH_REQUIRED')
  if (session.account.enterpriseId) throw failure('UNSUPPORTED_ACCOUNT')
  if (!Number.isFinite(session.auth.expiresAt) || session.auth.expiresAt <= now() + 5000) throw failure('AUTH_EXPIRED')
  let config
  try { config = await readProductConfig(configDir, io) }
  catch { throw failure('UNSUPPORTED_RUNTIME') }
  // Never send credentials to an arbitrary configured URL, private deployment,
  // staging server, redirect target, or another account's cached tenant.
  const endpoint = config?.endpoint
  if (!ORIGINS.has(endpoint) || config.enterpriseId || config.authentication?.id !== 'workbuddy-desktop') throw failure('UNSUPPORTED_RUNTIME')
  const accountKey = createHash('sha256').update('workbuddy:' + endpoint + ':' + session.account.uid).digest('hex')
  if (kind === 'identity') return { accountKey }
  if (['checkin', 'checkin-status'].includes(kind) && (endpoint !== 'https://www.workbuddy.cn' || expectedAccountKey !== accountKey)) throw failure(expectedAccountKey !== accountKey ? 'SESSION_CHANGED' : 'UNSUPPORTED_ACCOUNT')
  const runtime = loadRuntime(root, configDir)
  let dispatcher
  let token
  try {
    if (kind === 'models' && !runtime.userAgent) throw failure('UNSUPPORTED_RUNTIME')
    token = decodeField(session.auth.accessToken, runtime, '/auth/accessToken')
    if (!token || token.length > 65536 || /[\r\n]/.test(token)) throw failure('AUTH_REQUIRED')
    const identity = session.account.nickname ? decodeField(session.account.nickname, runtime, '/account/nickname') : ''
    // A dedicated plain Agent guarantees direct requests, independently of
    // MeterNest proxy settings or any global/environment proxy dispatcher.
    dispatcher = new runtime.Agent()
    const headers = { Authorization: `Bearer ${token}`, 'X-User-Id': String(session.account.uid),
      'Content-Type': 'application/json', Accept: 'application/json', 'Cache-Control': 'no-cache' }
    if (session.auth.domain) headers['X-Domain'] = String(session.auth.domain)
    const request = async (url, body = {}) => {
      const models = kind === 'models' && url === '/v3/config'
      if (!models && ![SUMMARY_PATH, ...(kind === 'summary' ? [] : [PAID_PATH, FREE_PATH]), ...(kind === 'checkin' ? [STATUS_PATH, CLAIM_PATH] : kind === 'checkin-status' ? [STATUS_PATH] : [])].includes(url)) throw failure('NETWORK_ERROR')
      const response = await runtime.fetch(endpoint + url, {
        method: models ? 'GET' : 'POST', headers: models ? { ...headers, 'User-Agent': runtime.userAgent || 'MeterNest', 'X-Product': config.deploymentType || 'SaaS', 'X-Requested-With': 'XMLHttpRequest' } : headers,
        ...(models ? {} : { body: JSON.stringify(body) }), dispatcher, redirect: 'error', credentials: 'omit', signal: AbortSignal.timeout(10000)
      })
      if ([401, 403].includes(response.status)) throw failure('AUTH_EXPIRED')
      if (!response.ok) throw failure('NETWORK_ERROR')
      const reader = response.body.getReader()
      let size = 0
      const chunks = []
      try {
        for (;;) {
          const { done, value } = await reader.read()
          if (done) break
          size += value.byteLength
          if (size > (models ? 4 : 1) * 1024 * 1024) throw failure('NETWORK_ERROR')
          chunks.push(Buffer.from(value))
        }
      } finally { await reader.cancel().catch(() => {}) }
      return JSON.parse(Buffer.concat(chunks).toString('utf8'))
    }
    async function validateSession() {
      if (await loggedOut(authPath, io)) throw failure('AUTH_REQUIRED')
      const current = await readJson(authPath, io)
      if (current.account?.uid !== session.account.uid || JSON.stringify(current.auth?.accessToken) !== JSON.stringify(session.auth.accessToken)) throw failure('SESSION_CHANGED')
    }
    let account
    if (['checkin', 'checkin-status'].includes(kind)) {
      account = { accountKey, ...await checkin({ request, validateSession, summaryPath: SUMMARY_PATH, statusOnly: kind === 'checkin-status' }) }
    } else if (kind === 'models') {
      const result = await request('/v3/config')
      const remote = result?.data
      if (!Array.isArray(remote?.models) || !remote.models.length) throw failure('NETWORK_ERROR')
      // Only fresh server models are used. Local agents supply the desktop's
      // visibility/order rules when omitted by the remote overlay.
      account = { models: normalizeModels({ ...remote, agents: remote.agents || config.agents,
        productFeatures: { ...remote.productFeatures, EnableAutoModelTiers: config.productFeatures?.EnableAutoModelTiers ?? remote.productFeatures?.EnableAutoModelTiers } }) }
      if (!account.models.length) throw failure('NETWORK_ERROR')
      await attachClientIcons(root, account.models, io)
    } else {
      const summary = await request(SUMMARY_PATH)
      account = normalizeSummary(summary, identity)
      try { if (kind !== 'summary') Object.assign(account, await fetchCreditData(request, summary, now())) }
      catch (error) {
        if (error.code === 'AUTH_EXPIRED') throw error
        account.detailsError = '积分明细暂不可用，请刷新后重试。'
      }
    }
    if (await loggedOut(authPath, io)) throw failure('AUTH_REQUIRED')
    // Recheck the source before returning: logout/account switch during a query
    // must not display the previous user's balance.
    const latest = await readJson(authPath, io)
    if (latest.account?.uid !== session.account.uid || !latest.auth?.accessToken) throw failure('AUTH_REQUIRED')
    // WorkBuddy can rotate a token during this read-only query. Never return
    // the old response; reread fresh credentials without refreshing them here.
    if (JSON.stringify(latest.auth.accessToken) !== JSON.stringify(session.auth.accessToken)) throw failure('SESSION_CHANGED')
    return account
  } catch (error) {
    if (Object.hasOwn(MESSAGES, error.code)) throw error
    throw failure('NETWORK_ERROR')
  } finally {
    token = null
    session = null
    if (runtime.dispose) runtime.dispose()
    else runtime.codec?.dispose()
    await dispatcher?.destroy().catch(() => {})
  }
}

if (require.main === module) {
  // Installed modules must not print diagnostic details or secrets to stdout.
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) console[level] = () => {}
  let input = ''
  process.stdin.setEncoding('utf8')
  process.stdin.on('data', chunk => { input += chunk; if (input.length > 16384) process.exit(1) })
  process.stdin.on('end', async () => {
    let result
    try { result = { ok: true, account: await query(JSON.parse(input)) } }
    catch (error) { result = { ok: false, code: Object.hasOwn(MESSAGES, error.code) ? error.code : 'UNSUPPORTED_RUNTIME' } }
    process.stdout.write(JSON.stringify(result) + '\n', () => process.exit(0))
  })
}
module.exports = { query, failure, MESSAGES, ORIGINS, loadLegacyCrypto, loadModernCrypto }
