// The caller is the isolated WorkBuddy runtime. It alone holds request headers.
const { actionError } = require('./errors')
const STATUS_PATH = '/v2/billing/meter/checkin-activity-status'
const CLAIM_PATH = '/v2/billing/meter/daily-checkin'
const terminal = code => ({ 1001: 'done', 1002: 'ineligible', 1003: 'inactive' })[Number(code)]
function unpack(value) {
  const outcome = terminal(value?.code)
  if (outcome) return { outcome }
  if (!value || typeof value !== 'object' || value.code !== undefined && Number(value.code) !== 0) throw actionError('NETWORK_ERROR')
  return value.data && typeof value.data === 'object' ? value.data : value
}
const credits = value => Number.isFinite(Number(value)) && Number(value) >= 0 ? Number(value) : 0
async function checkin({ request, validateSession, summaryPath, statusOnly = false }) {
  try {
    const status = unpack(await request(STATUS_PATH))
    if (status.outcome) return status
    if (status.active === false) return { outcome: 'inactive' }
    if (status.active !== true || ![true, false, 0, 1].includes(status.today_checked_in)) throw actionError('NETWORK_ERROR')
    if (status.today_checked_in) return { outcome: 'done', credit: credits(status.today_credit), streak: credits(status.streak_days) }
    if (statusOnly) return { outcome: 'pending' }
    await validateSession()
    const result = unpack(await request(CLAIM_PATH))
    if (result.outcome) return result
    if (!Object.hasOwn(result, 'credit') || !Number.isFinite(Number(result.credit)) || Number(result.credit) < 0) throw actionError('NETWORK_ERROR')
    return { outcome: 'claimed', credit: Number(result.credit), streak: credits(result.streak_days) }
  } catch (error) {
    if (error.code === 'AUTH_EXPIRED') {
      // Credits can work while the claim requires the vendor's device SDK.
      try { unpack(await request(summaryPath)); return { outcome: 'own-app' } } catch {}
    }
    throw error
  }
}
module.exports = { checkin, STATUS_PATH, CLAIM_PATH }
