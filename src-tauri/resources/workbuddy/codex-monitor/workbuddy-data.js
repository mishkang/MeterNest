const SUMMARY_PATH = '/billing/meter/get-user-resource-summary'
const PAID_PATH = '/billing/meter/get-user-resource-paid-packages'
const FREE_PATH = '/billing/meter/get-user-resource-free-packages'
// Exact product codes used by WorkBuddy's own billing client, not display-name matching.
const codes = {
  base: ['002_AkiJS3ZHF5', '005_maRGyrHhw1', '003_FAnt7lcmRT', '023_4xbGhMrE6q', '026_BaESVICNoi', '027_0FCGVA6vSa', '008_cfWoLwvjU4', '035_ArVxJcGDsm', '039_KRcQj7wUat', '040_mi9rCYg46x', '001_PqouKr6QWV', '006_DbXS0lrypC'],
  reward: ['007_nzdH5h4Nl0', '029_6wCGEWquYy', '030_BjSt89qTvr'],
  bonus: ['028_NtpWi0jzXs', '037_WxOD3MpI2o'],
  purchased: ['009_0XmEQc2xOf', '038_OhvqZtiPKr', '036_lupO5WgNdG']
}
const kinds = new Map(Object.entries(codes).flatMap(([kind, items]) => items.map(code => [`TCACA_code_${code}`, kind])))
const paidCodes = new Set(['002_AkiJS3ZHF5', '005_maRGyrHhw1', '003_FAnt7lcmRT', '023_4xbGhMrE6q', '026_BaESVICNoi', '027_0FCGVA6vSa', ...codes.purchased].map(code => `TCACA_code_${code}`))
const labels = { base: '套餐基础积分', reward: '平台奖励积分', purchased: '购买积分', bonus: '套餐赠送积分', other: '其他积分' }
const category = code => kinds.get(code) || 'other'
function count(value) {
  if (!['string', 'number'].includes(typeof value) || String(value).trim() === '') throw new Error('WorkBuddy 积分格式已变化')
  const number = Number(value)
  if (!Number.isFinite(number) || number < 0) throw new Error('WorkBuddy 积分格式已变化')
  return number
}
function billingTime(value) {
  if (value == null || value === '') return null
  let time
  if (typeof value === 'number' || /^\d+$/.test(value)) {
    const number = Number(value)
    time = number < 1e12 ? number * 1000 : number
  } else {
    const text = String(value).trim().replace(' ', 'T')
    time = Date.parse(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?$/.test(text) ? `${text}+08:00` : text)
  }
  return Number.isFinite(time) && time > 0 && time < 8640000000000000 ? time : null
}
function normalizeSummary(envelope, identity = '') {
  if (envelope?.code !== 0 || !Array.isArray(envelope.data?.Packages)) throw new Error('WorkBuddy 积分响应暂不可用')
  const breakdown = { base: 0, reward: 0, purchased: 0, bonus: 0, other: 0 }
  let balance = 0
  for (const item of envelope.data.Packages) {
    const value = count(item?.CycleRemainCapacity)
    balance += value
    breakdown[category(item.PackageCode)] += value
  }
  if (!Number.isFinite(balance)) throw new Error('WorkBuddy 积分格式已变化')
  return { status: 'ready', identity: typeof identity === 'string' ? identity.slice(0, 200) : '',
    plan: typeof envelope.data.SubscriptionPackageName === 'string' ? envelope.data.SubscriptionPackageName.slice(0, 100) : '', balance, breakdown }
}
function normalizeDetails(resources, now = Date.now(), expired = false) {
  const seen = new Set()
  const details = []
  for (const resource of resources) {
    if (resource.CapacityType === 4 || !(expired ? [2, 3] : [0, 3, undefined]).includes(resource.Status)) continue
    const expires = billingTime(resource.DeductionEndTime) || billingTime(resource.ExpiredTime) || billingTime(resource.CycleEndTime)
    const starts = billingTime(resource.DeductionStartTime) || billingTime(resource.CycleStartTime)
    if (!expires || (expired ? expires > now : expires <= now) || (starts && starts > now)) continue
    const total = count(expired ? resource.CapacitySizePrecise ?? resource.CycleCapacitySizePrecise : resource.CycleCapacitySizePrecise)
    const remaining = count(expired ? resource.CapacityRemainPrecise ?? resource.CycleCapacityRemainPrecise : resource.CycleCapacityRemainPrecise)
    if (!total) continue
    const usedValue = expired ? resource.CapacityUsedPrecise ?? resource.CycleCapacityUsedPrecise : resource.CycleCapacityUsedPrecise
    const used = usedValue == null ? Math.max(0, total - remaining) : count(usedValue)
    if (remaining > total) throw new Error('WorkBuddy 积分明细格式已变化')
    const id = resource.ResourceId
    if (typeof id !== 'string' || !id) throw new Error('WorkBuddy 积分明细标识缺失')
    if (seen.has(id)) continue
    seen.add(id)
    const kind = category(resource.PackageCode)
    details.push({ kind, name: labels[kind], total, remaining, used, expiresAt: new Date(expires).toISOString() })
  }
  return details.sort((a, b) => (Date.parse(a.expiresAt) - Date.parse(b.expiresAt)) * (expired ? -1 : 1))
}
async function fetchCreditDetails(request, summary, now = Date.now(), expired = false) {
  const packageCodes = [...new Set([...summary.data.Packages.map(item => item.PackageCode), ...(expired ? kinds.keys() : [])].filter(code => typeof code === 'string' && code))]
  if (!packageCodes.length && summary.data.Packages.length) throw new Error('WorkBuddy 积分明细分类暂不可用')
  const periodBegin = new Date(now)
  periodBegin.setUTCFullYear(periodBegin.getUTCFullYear() - 101)
  const groups = [
    [PAID_PATH, packageCodes.filter(code => paidCodes.has(code) || !kinds.has(code))],
    [FREE_PATH, packageCodes.filter(code => !paidCodes.has(code))]
  ]
  const results = await Promise.all(groups.map(async ([url, PackageCodes]) => {
    if (!PackageCodes.length) return []
    const rows = []
    let previousPage
    for (let page = 1; ; page++) {
      const result = await request(url, { PageNumber: page, PageSize: 200, PackageCodes, ...(expired ? { Status: [2, 3], PackageEndTimeRangeBegin: beijingTime(periodBegin.getTime()), PackageEndTimeRangeEnd: beijingTime(now), OrderBy: 'endTime', SortBy: 'desc' } : { Status: [0, 3], OnlyValidPeriod: true }), IsDisplayTotalInfo: true })
      if (result?.code !== 0 || !Array.isArray(result.data?.Accounts)) throw new Error('WorkBuddy 积分明细暂不可用')
      const accounts = result.data.Accounts
      const currentPage = JSON.stringify(accounts)
      if (accounts.length && currentPage === previousPage) throw new Error('WorkBuddy 积分明细分页重复')
      previousPage = currentPage
      rows.push(...accounts)
      const total = result.data.TotalCount == null ? NaN : Number(result.data.TotalCount)
      const more = Number.isFinite(total) && total >= 0 ? rows.length < total : result.data.Accounts.length >= 200
      if (!more) return rows
      if (!result.data.Accounts.length) throw new Error('WorkBuddy 积分明细分页不完整')
    }
  }))
  return normalizeDetails(results.flat(), now, expired)
}
function beijingTime(time) {
  return new Date(time + 8 * 3600000).toISOString().slice(0, 19).replace('T', ' ')
}
async function fetchCreditData(request, summary, now = Date.now()) {
  const data = {}
  await Promise.all([
    ['creditDetails', 'detailsError', () => fetchCreditDetails(request, summary, now)],
    ['expiredCreditDetails', 'expiredDetailsError', () => fetchCreditDetails(request, summary, now, true)]
  ].map(async ([key, errorKey, read]) => {
    try { data[key] = await read() }
    catch (error) { if (error.code === 'AUTH_EXPIRED') throw error; data[errorKey] = '明细暂不可用，请刷新后重试。' }
  }))
  return data
}
// Keep only bounded display fields when a child process returns account data.
function displayCreditData(account) {
  const data = {}
  if (account.breakdown) {
    data.breakdown = Object.fromEntries(Object.keys(labels).map(kind => [kind, count(account.breakdown[kind])]))
  }
  for (const key of ['creditDetails', 'expiredCreditDetails']) if (Array.isArray(account[key])) data[key] = account[key].map(item => {
    const kind = Object.hasOwn(labels, item.kind) ? item.kind : 'other'
    const expiresAt = billingTime(item.expiresAt)
    if (!expiresAt) throw new Error('WorkBuddy 积分明细日期无效')
    return { kind, name: labels[kind], total: count(item.total), remaining: count(item.remaining), used: count(item.used), expiresAt: new Date(expiresAt).toISOString() }
  })
  for (const key of ['detailsError', 'expiredDetailsError']) if (account[key]) data[key] = '明细暂不可用，请刷新后重试。'
  return data
}
module.exports = { fetchCreditData, SUMMARY_PATH, PAID_PATH, FREE_PATH, normalizeSummary, normalizeDetails, fetchCreditDetails, displayCreditData, billingTime }
