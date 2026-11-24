// Shared pure display rules: usable in the helper, main process and local UI.
const text = (value, max = 160) => typeof value === 'string' ? value.trim().slice(0, max) : ''
function modelIconUrl(value) {
  if (typeof value !== 'string') return ''
  value = value.trim()
  if (value.length > 131072) return ''
  if (/^data:image\/(?:svg\+xml|png|jpeg|gif|webp|avif)(?:;[a-z0-9=.+-]+)*,.+/i.test(value)) return value
  if (value.length > 4096) return ''
  try {
    const url = new URL(value)
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : ''
  } catch { return '' }
}
function multiplier(value) {
  if (typeof value !== 'string') return null
  const match = /^(?:x\s*(\d+(?:\.\d+)?)|(\d+(?:\.\d+)?)\s*x)$/i.exec(value.trim())
  const number = match && Number(match[1] || match[2])
  return match && Number.isFinite(number) && number >= 0 ? number : null
}
function sanitizeSchedule(value) {
  if (!value || typeof value !== 'object') return undefined
  return { timezone: text(value.timezone, 80) || 'Asia/Shanghai', validFrom: text(value.validFrom, 50), validUntil: text(value.validUntil, 50),
    daily: Array.isArray(value.daily) ? value.daily.slice(0, 24).map(item => ({ start: text(item?.start, 5), end: text(item?.end, 5) })) : [] }
}
function sanitizePromotion(item) {
  if (!item || item.enabled === false) return null
  const discount = item.discount
  return { id: text(item.id), priority: Number.isFinite(item.priority) ? item.priority : 0, kind: text(item.kind, 30),
    label: text(item.badge?.label || item.label, 40), always: item.badge?.display === 'always' || item.always === true,
    description: text(item.hover?.textZh || item.description, 600), schedule: sanitizeSchedule(item.schedule),
    discount: discount ? { factor: Number.isFinite(discount.factor) ? discount.factor : null, discountedCredits: text(discount.discountedCredits, 30),
      displayMode: discount.displayMode === 'replace' ? 'replace' : 'strikethrough' } : null }
}
function normalizeModels(config) {
  if (!Array.isArray(config?.models) || config.models.length > 1000) throw new Error('模型配置格式无效')
  const agents = Array.isArray(config.agents) ? config.agents : []
  const cli = agents.find(agent => agent.tags?.includes('default')) || agents.find(agent => agent.name === 'cli') || agents.find(agent => agent.models?.length)
  const order = Array.isArray(cli?.models) ? cli.models.flatMap(id => id === 'auto' && config.productFeatures?.EnableAutoModelTiers
    ? ['fast-model', 'balanced-model', 'deep-model'] : [id]) : []
  // Match the client's selectConversationModels: resolve each entry in order
  // by ID or name, then deduplicate. Never sort by rate, badge or API order.
  const candidates = cli ? order.map(key => config.models.find(model => model?.id === key || model?.name === key)).filter(Boolean) : config.models
  const allowed = Array.isArray(config.availableModels) && config.availableModels.length ? new Set(config.availableModels) : null
  const seen = new Set()
  const paidLines = config.productFeaturesConfig?.ModelRateLimitCap?.lines
  const hiddenPaid = new Set(Array.isArray(paidLines) ? paidLines.filter(line => config.models.some(model => model.id === line.freeId)).map(line => line.paidId) : [])
  const models = candidates.filter(model => {
    const id = text(model?.id)
    if (!id || seen.has(id) || hiddenPaid.has(id) || /^(custom|custom-local)[:/]/i.test(id) || model.custom || model.isCustom || model.hidden || model.disabled || model.tags?.includes('custom')) return false
    if (allowed && !allowed.has(id)) return false
    seen.add(id)
    return true
  }).map(model => {
    const promotions = [...(Array.isArray(model.promotions) ? model.promotions : []), ...(config.modelPromotions || []).filter(item => item.modelIds?.includes(model.id))]
    const tiers = [...(Array.isArray(model.tiers) ? model.tiers : []), ...(config.modelTiers || []).filter(item => item.modelIds?.includes(model.id))]
    return { id: text(model.id), name: text(model.name) || text(model.id), credits: text(model.credits, 30),
      iconTemplate: model.iconTemplate === true,
      iconUrl: modelIconUrl(model.iconUrl),
      description: text(model.descriptionZh || model.description || model.descriptionEn, 1200),
      promotions: promotions.slice(0, 30).map(sanitizePromotion).filter(Boolean),
      tiers: tiers.filter(item => item?.enabled !== false).slice(0, 10).map(item => ({ label: text(item.badge?.label || item.label, 40), description: text(item.hover?.textZh || item.description, 600) })) }
  })
  return models
}
function activePromotion(item, now = Date.now()) {
  const schedule = item.schedule
  if (!schedule) return true
  if (schedule.validFrom && (!Number.isFinite(Date.parse(schedule.validFrom)) || now < Date.parse(schedule.validFrom))) return false
  if (schedule.validUntil && (!Number.isFinite(Date.parse(schedule.validUntil)) || now >= Date.parse(schedule.validUntil))) return false
  if (!schedule.daily?.length) return true
  let minute
  try {
    const parts = new Intl.DateTimeFormat('en-US', { timeZone: schedule.timezone || 'Asia/Shanghai', hourCycle: 'h23', hour: '2-digit', minute: '2-digit' }).formatToParts(new Date(now))
    minute = Number(parts.find(item => item.type === 'hour').value) * 60 + Number(parts.find(item => item.type === 'minute').value)
  } catch { return false }
  const parse = value => {
    const match = /^(\d{1,2}):(\d{2})$/.exec(value || '')
    return match && Number(match[1]) < 24 && Number(match[2]) < 60 ? Number(match[1]) * 60 + Number(match[2]) : null
  }
  return schedule.daily.some(window => {
    const start = parse(window.start), end = parse(window.end)
    return start !== null && end !== null && (start === end || (start < end ? minute >= start && minute < end : minute >= start || minute < end))
  })
}
function modelDisplay(model, now = Date.now()) {
  const promotions = [...model.promotions].sort((a, b) => b.priority - a.priority)
  const promotion = promotions.find(item => activePromotion(item, now)) || promotions[0]
  const active = promotion && activePromotion(promotion, now)
  const base = multiplier(model.credits)
  const format = value => value === null ? '—' : `${value.toFixed(Math.max(2, Math.min(4, String(value).split('.')[1]?.length || 0)))}x`
  let current = base
  if (active && promotion.kind === 'discount' && promotion.discount) {
    const explicit = multiplier(promotion.discount.discountedCredits), factor = promotion.discount.factor
    if (explicit !== null) current = explicit
    else if (base !== null && Number.isFinite(factor) && factor >= 0 && factor < 1) current = Number((base * factor).toFixed(4))
  }
  const badges = []
  if (promotion?.label && (active || promotion.always)) badges.push({ label: promotion.label, description: promotion.description, kind: 'promotion' })
  for (const tier of model.tiers) if (tier.label && !badges.some(item => item.label === tier.label)) badges.push({ ...tier, kind: 'tier' })
  const descriptions = [text(model.description, 1200)]
  if (active && promotion.description && !descriptions[0].includes(promotion.description)) descriptions.push(promotion.description)
  for (const badge of badges) if (badge.description && !descriptions.some(value => value.includes(badge.description))) descriptions.push(badge.description)
  return { name: model.name, iconTemplate: model.iconTemplate === true, iconUrl: modelIconUrl(model.iconUrl), description: descriptions.filter(Boolean).join('\n').replace(/\r\n?/g, '\n').replace(/\n[\t ]*\n+/g, '\n'), credits: format(current), original: active && promotion.discount?.displayMode !== 'replace' && base !== current ? format(base) : '', badges }
}
const modelRules = { normalizeModels, modelDisplay, activePromotion, multiplier }
if (typeof module === 'object' && module.exports) module.exports = modelRules
else globalThis.WorkBuddyModels = modelRules
