const api = window.codexMonitor
const refresh = document.getElementById('refresh')
let currentState
let selectedProvider = 'codex'
let creditPeriod = 'valid'
const CREDIT_PAGE_SIZE = 5
const creditPages = { valid: 1, expired: 1 }
let selectionRevision = 0
const formatDate = (value, compact) => value ? new Intl.DateTimeFormat(i18n.language, {
  timeZone: 'Asia/Shanghai', ...(compact ? {} : {year: 'numeric', second: '2-digit'}),
  month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23'
}).format(new Date(typeof value === 'number' ? value * 1000 : value)) : i18n.t('暂不可用')
const date = value => formatDate(value, false)
const compactDate = value => formatDate(value, true)
const credits = value => value.toLocaleString(i18n.language, { maximumFractionDigits: 2 })
function element(tag, className, text) {
  const node = document.createElement(tag)
  if (className) node.className = className
  if (text != null) node.textContent = text
  return node
}
function duration(minutes) {
  if (!minutes) return i18n.t('额度窗口')
  if (minutes % 10080 === 0) return i18n.t("{0} 周额度", minutes / 10080)
  if (minutes % 1440 === 0) return i18n.t("{0} 天额度", minutes / 1440)
  if (minutes % 60 === 0) return i18n.t("{0} 小时额度", minutes / 60)
  return i18n.t("{0} 分钟额度", minutes)
}
function remaining(at) {
  if (!at) return ''
  const minutes = Math.ceil((at * 1000 - Date.now()) / 60000)
  if (minutes <= 0) return i18n.t(' · 已到恢复时间，刷新确认')
  if (minutes >= 1440) return i18n.t(" · 约 {0} 天 {1} 小时后", Math.floor(minutes / 1440), Math.floor(minutes % 1440 / 60))
  return i18n.t(" · 约 {0} 小时 {1} 分后", Math.floor(minutes / 60), minutes % 60)
}
function updateTime(updatedAt, loading, error) {
  if (!updatedAt) return loading ? i18n.t('正在查询…') : error ? i18n.t('更新失败') : ''
  return i18n.t("更新于 {0}{1}", date(updatedAt), loading ? i18n.t(' · 正在更新') : error ? i18n.t(' · 更新失败') : '')
}
function renderUsage(state) {
  const container = document.getElementById('usage')
  container.replaceChildren()
  const account = state.account
  document.getElementById('identity').textContent = account?.identity || i18n.t('通过本机 Codex 只读接口获取')
  document.getElementById('plan').textContent = i18n.t(account?.plan || '本机')
  if (!account || account.status !== 'ready') container.append(element('p', 'empty', i18n.message(state.accountError || account?.message) || i18n.t('正在读取额度…')))
  else if (state.accountError) container.append(element('p', 'empty', i18n.message(state.accountError)))
  for (const bucket of account?.buckets || []) {
    if (account.buckets.length > 1) container.append(element('p', 'bucket-name', bucket.name))
    for (const quota of [bucket.primary, bucket.secondary].filter(Boolean)) {
      const item = element('div', `quota${quota.remainingPercent <= 15 ? ' low' : ''}`)
      const heading = element('div', 'quota-heading')
      heading.append(element('span', '', duration(quota.windowDurationMins)), element('strong', '', i18n.t("{0}% 剩余", quota.remainingPercent)))
      const track = element('div', 'track')
      const fill = element('div', 'fill')
      fill.style.width = `${quota.remainingPercent}%`
      track.append(fill)
      const recovery = element('div', 'quota-time', i18n.t("恢复：{0}{1}", compactDate(quota.resetsAt), remaining(quota.resetsAt)))
      recovery.title = date(quota.resetsAt)
      item.append(heading, track, recovery)
      container.append(item)
    }
  }
  document.getElementById('reset-credits').textContent = account?.resetCredits != null ? i18n.t("可用重置次数：{0}", account.resetCredits) : ''
  document.getElementById('usage-time').title = date(state.updatedAt)
  document.getElementById('usage-time').textContent = updateTime(state.updatedAt, state.accountLoading ?? state.loading, state.accountError)
}
const expandedRecords = new Set()
function renderRecords(state) {
  for (const id of expandedRecords) if (!state.records.some(record => record.id === id)) expandedRecords.delete(id)
  const container = document.getElementById('records')
  container.replaceChildren()
  for (const record of state.records) {
    const card = element('article', 'record')
    const complete = record.stage === 'completed'
    const displayAt = complete ? record.completedAt : record.announcedAt
    const time = element('time', '', `${complete ? i18n.t('完成时间') : i18n.t('公告时间')}：${date(displayAt)}`)
    if (displayAt) time.dateTime = displayAt
    const heading = element('div', 'record-title')
    heading.append(element('h3', '', record.title), element('span', `badge ${complete ? 'done' : 'pending'}`, complete ? i18n.t('已完成') : i18n.t('待确认')))
    const posts = (record.posts || []).filter(post => post.translation)
    const latest = element('div', 'record-latest')
    latest.append(element('div', 'message-meta', posts.length ? i18n.t('最新译文') : i18n.t('事件摘要')),
      element('p', 'message-text', posts.length ? posts[posts.length - 1].translation : record.summary))
    card.append(time, heading, latest)
    if (posts.length > 1) {
      const details = element('details', 'record-messages')
      details.open = expandedRecords.has(record.id)
      const toggle = element('summary', '')
      toggle.append(element('span', 'when-closed', i18n.t("展开全部消息（{0} 条）", posts.length)), element('span', 'when-open', i18n.t('收起全部消息')))
      const messages = element('ol', 'message-list')
      for (const post of posts) {
        const row = element('li', '')
        const postedAt = element('time', '', post.at ? date(post.at) : i18n.t('发布时间暂不可用'))
        if (post.at) postedAt.dateTime = post.at
        const meta = element('div', 'message-meta')
        meta.append(postedAt, element('span', '', ({ announce: i18n.t('公告'), update: i18n.t('更新'), complete: i18n.t('完成') })[post.role] || ''))
        row.append(meta, element('p', 'message-text', post.translation))
        messages.append(row)
      }
      details.append(toggle, messages)
      details.addEventListener('toggle', () => {
        if (!details.isConnected) return
        if (details.open) expandedRecords.add(record.id)
        else expandedRecords.delete(record.id)
      })
      card.append(details)
    }
    const bottom = element('div', 'record-bottom')
    bottom.append(element('span', '', record.sources.map(source => source.name).join(' · ')))
    if (record.link) {
      const link = element('button', 'link', i18n.t('查看原文 ↗'))
      link.addEventListener('click', () => api.open(record.link).catch(() => {}))
      bottom.append(link)
    }
    card.append(bottom)
    container.append(card)
  }
  if (!state.records.length) container.append(element('p', 'empty', (state.feedLoading ?? state.loading) ? i18n.t('正在读取最新记录…') : i18n.t('记录暂不可用，可刷新或打开数据源查看。')))
  const failed = state.sources.filter(source => !source.ok)
  document.getElementById('feed-status').textContent = failed.length
    ? `${failed.map(source => `${source.name}：${i18n.message(source.error) || i18n.t('暂不可用')}`).join('；')}${state.records.length ? i18n.t("。显示缓存（上次成功更新于 {0}）。", date(state.feedUpdatedAt)) : '。'}`
    : (state.feedUpdatedAt ? i18n.t("来源：Codex Reset FM · {0}", updateTime(state.feedUpdatedAt, state.feedLoading ?? state.loading, false)) : '')
}
const modelTip = element('div', 'model-tip')
modelTip.id = 'model-description-tip'
modelTip.setAttribute('role', 'tooltip')
modelTip.hidden = true
document.body.append(modelTip)
function hideModelTip() { modelTip.hidden = true }
function showModelTip(name) {
  if (!name.dataset.description || !name.isConnected) return
  modelTip.textContent = name.dataset.description
  modelTip.hidden = false
  const anchor = name.getBoundingClientRect()
  const panel = document.body.getBoundingClientRect()
  const box = modelTip.getBoundingClientRect()
  const left = Math.min(anchor.left - panel.left, panel.width - box.width - 10)
  const below = anchor.bottom - panel.top + 6
  const top = below + box.height <= panel.height - 10 ? below : anchor.top - panel.top - box.height - 6
  modelTip.style.left = `${Math.max(10, left)}px`
  modelTip.style.top = `${Math.max(10, top)}px`
}
document.addEventListener('scroll', hideModelTip, true)
document.addEventListener('visibilitychange', hideModelTip)
document.addEventListener('keydown', event => { if (event.key === 'Escape') hideModelTip() })
window.addEventListener('blur', hideModelTip)
let modelFingerprint = ''
function modelValues(state) {
  return (state.models?.items || []).map(model => window.WorkBuddyModels.modelDisplay(model))
}
function createModelIcon(value, useClientImage = true) {
  const clientImage = useClientImage && value.iconUrl
  const icon = element(clientImage ? 'img' : 'span', 'model-icon')
  icon.setAttribute('aria-hidden', 'true')
  if (clientImage) {
    icon.classList.add('model-icon-image')
    if (value.iconTemplate) icon.classList.add('model-icon-template')
    icon.alt = ''
    icon.draggable = false
    icon.referrerPolicy = 'no-referrer'
    icon.addEventListener('error', () => {
      if (icon.isConnected) icon.replaceWith(createModelIcon(value, false))
    }, { once: true })
    icon.src = clientImage
  } else icon.classList.add('model-icon-fallback')
  return icon
}
function renderModels(state) {
  hideModelTip()
  const data = state.models || {}
  const list = document.getElementById('model-list')
  const scrollTop = list.scrollTop
  list.replaceChildren()
  document.getElementById('model-status').textContent = i18n.t("{0} · 每{1}分钟更新", updateTime(data.updatedAt, data.loading, data.error), state.refreshMinutes?.models || 10)
  if (data.error) list.append(element('p', 'empty', i18n.message(data.error)))
  const values = modelValues(state)
  modelFingerprint = JSON.stringify(values)
  for (const [index, model] of (data.items || []).entries()) {
    const value = values[index]
    const row = element('article', 'model-row')
    row.dataset.modelId = model.id
    const info = element('div', 'model-info')
    info.append(createModelIcon(value))
    const name = element('h3', 'model-name', value.name)
    if (value.description) {
      row.dataset.description = value.description
      row.tabIndex = 0
      row.setAttribute('aria-describedby', modelTip.id)
      row.addEventListener('pointerenter', () => showModelTip(row))
      row.addEventListener('pointermove', () => { if (modelTip.hidden) showModelTip(row) })
      row.addEventListener('pointerleave', hideModelTip)
      row.addEventListener('focus', () => showModelTip(row))
      row.addEventListener('blur', hideModelTip)
    }
    info.append(name)
    const badges = element('div', 'model-badges')
    for (const badge of value.badges) {
      const label = element('span', `badge model-badge ${badge.kind}`, badge.label)
      badges.append(label)
    }
    info.append(badges)
    row.append(info)
    const rate = element('div', 'model-rate')
    if (value.original) rate.append(element('s', 'muted', value.original))
    rate.append(element('span', '', value.credits))
    row.append(rate)
    list.append(row)
  }
  if (!list.children.length) list.append(element('p', 'empty', data.loading ? i18n.t('正在读取模型…') : i18n.t('暂无模型数据，可点击刷新。')))
  list.scrollTop = scrollTop
}
function renderWorkBuddy(state) {
  const data = state.workbuddy || {}
  const account = data.account
  document.getElementById('workbuddy-identity').textContent = account?.identity || i18n.t('本机 WorkBuddy 账号')
  document.getElementById('workbuddy-plan').textContent = i18n.t(account?.plan || 'WorkBuddy')
  const usage = document.getElementById('workbuddy-usage')
  usage.replaceChildren()
  if (account?.status === 'ready') {
    if (data.error) usage.append(element('p', 'empty', i18n.message(data.error)))
    const balance = element('div', 'credit-balance')
    balance.append(element('strong', '', account.balance.toLocaleString(i18n.language, { minimumFractionDigits: 2, maximumFractionDigits: 2 })), element('span', 'muted', i18n.t('总剩余积分')))
    usage.append(balance)
    const categories = element('div', 'credit-categories')
    for (const [kind, label] of [['base', i18n.t('套餐基础积分')], ['reward', i18n.t('平台奖励积分')], ['purchased', i18n.t('购买积分')], ['bonus', i18n.t('套餐赠送积分')], ['other', i18n.t('其他积分')]]) {
      if (['bonus', 'other'].includes(kind) && !account.breakdown?.[kind]) continue
      const item = element('div', 'credit-category')
      item.dataset.kind = kind
      item.append(element('span', 'muted', label), element('strong', '', account.breakdown ? credits(account.breakdown[kind]) : '—'))
      categories.append(item)
    }
    usage.append(categories)
  } else usage.append(element('p', 'empty', i18n.message(data.error) || (data.loading ? i18n.t('正在读取积分…') : i18n.t('积分暂不可用，可手动刷新。'))))
  const historyVisible = state.showWorkBuddyHistory !== false
  document.getElementById('workbuddy-history').hidden = !historyVisible
  if (!historyVisible) {
    creditPeriod = 'valid'; creditPages.valid = 1; creditPages.expired = 1; hideModelTip()
    document.getElementById('workbuddy-time').title = date(data.updatedAt)
    document.getElementById('workbuddy-time').textContent = updateTime(data.updatedAt, data.loading, data.error)
    return
  }
  const details = document.getElementById('workbuddy-details')
  const detailScrollTop = details.scrollTop
  details.replaceChildren()
  document.querySelectorAll('[data-credit-period]').forEach(button => button.setAttribute('aria-pressed', String(button.dataset.creditPeriod === creditPeriod)))
  const isModels = creditPeriod === 'models'
  details.hidden = isModels
  document.getElementById('workbuddy-models').hidden = !isModels
  if (isModels) {
    document.getElementById('credit-pagination').hidden = true
    renderModels(state)
    document.getElementById('workbuddy-time').title = date(data.updatedAt)
    document.getElementById('workbuddy-time').textContent = updateTime(data.updatedAt, data.loading, data.error)
    return
  }
  const expired = creditPeriod === 'expired'
  const now = Date.now()
  const list = expired ? account?.expiredCreditDetails || [] : (account?.creditDetails || [])
    .filter(item => Date.parse(item.expiresAt) > now)
    .sort((a, b) => (b.remaining > 0) - (a.remaining > 0) || Date.parse(a.expiresAt) - Date.parse(b.expiresAt))
  const pageCount = Math.max(1, Math.ceil(list.length / CREDIT_PAGE_SIZE))
  const page = Math.min(Math.max(1, creditPages[creditPeriod]), pageCount)
  creditPages[creditPeriod] = page
  for (const item of list.slice((page - 1) * CREDIT_PAGE_SIZE, page * CREDIT_PAGE_SIZE)) {
    const at = Date.parse(item.expiresAt)
    const row = element('article', 'credit-detail')
    const heading = element('div', 'credit-detail-title')
    heading.append(element('h3', '', i18n.t(item.name)))
    const hours = (at - now) / 3600000
    if (!expired && hours <= 24) heading.append(element('span', 'badge critical', i18n.t('24小时内到期')))
    else if (!expired && hours <= 168) heading.append(element('span', 'badge pending', i18n.t("{0}天内到期", Math.ceil(hours / 24))))
    heading.append(element('span', 'credit-remaining', i18n.t("剩余 {0}", credits(item.remaining))))
    const meta = element('div', 'credit-detail-meta')
    const expiry = element('time', '', i18n.t("到期时间 {0}", date(item.expiresAt)))
    expiry.title = date(item.expiresAt)
    meta.append(expiry, element('span', '', i18n.t("已用 {0}/{1}", credits(item.used), credits(item.total))))
    const track = element('div', 'track credit-track')
    const fill = element('div', 'fill')
    fill.style.width = `${Math.max(0, Math.min(100, item.total > 0 ? item.used / item.total * 100 : 0))}%`
    track.append(fill)
    row.append(heading, meta, track)
    details.append(row)
  }
  if (!details.children.length) details.append(element('p', 'empty', i18n.message(expired ? account?.expiredDetailsError : account?.detailsError) || (data.loading ? i18n.t('正在读取明细…') : account?.status !== 'ready' ? i18n.t('查询成功后展示积分明细。') : !Array.isArray(expired ? account.expiredCreditDetails : account.creditDetails) ? i18n.t('积分明细暂不可用，请刷新后重试。') : i18n.t(expired ? '暂无已过期积分。' : '暂无有效期内积分。'))))
  document.getElementById('credit-pagination').hidden = pageCount <= 1
  document.getElementById('credit-prev').disabled = page <= 1
  document.getElementById('credit-next').disabled = page >= pageCount
  document.getElementById('credit-page').textContent = i18n.t("第 {0} / {1} 页", page, pageCount)
  details.scrollTop = detailScrollTop
  document.getElementById('workbuddy-time').title = date(data.updatedAt)
  document.getElementById('workbuddy-time').textContent = updateTime(data.updatedAt, data.loading, data.error)
}
// Keep both provider cards at the same height so switching tabs never shifts
// the credit/history sections. Measure natural heights with both panels
// visible, then apply the taller one to both (never clipping either card).
function syncAccountCardHeights() {
  const codexPanel = document.getElementById('codex-content')
  const workbuddyPanel = document.getElementById('workbuddy-content')
  const codexCard = codexPanel.querySelector('.account-card')
  const workbuddyCard = workbuddyPanel.querySelector('.account-card')
  const codexHidden = codexPanel.hidden
  const workbuddyHidden = workbuddyPanel.hidden
  codexCard.style.height = ''
  workbuddyCard.style.height = ''
  if (currentState?.enabledProviders && !(currentState.enabledProviders.codex && currentState.enabledProviders.workbuddy)) return
  codexPanel.hidden = false
  workbuddyPanel.hidden = false
  const height = Math.ceil(Math.max(codexCard.offsetHeight, workbuddyCard.offsetHeight))
  codexPanel.hidden = codexHidden
  workbuddyPanel.hidden = workbuddyHidden
  if (height > 0) {
    codexCard.style.height = `${height}px`
    workbuddyCard.style.height = `${height}px`
  }
}
function render(state) {
  hideModelTip()
  currentState = state
  const enabled = state.enabledProviders || { codex: true, workbuddy: true }
  if (!enabled[selectedProvider]) {
    resetProviderView()
    selectedProvider = enabled[state.provider] ? state.provider : enabled.codex ? 'codex' : 'workbuddy'
    selectionRevision++
  }
  document.querySelector('.provider-tabs').hidden = !(enabled.codex && enabled.workbuddy)
  const minutes = state.refreshMinutes || { codex: 1, workbuddy: 5, feed: 10 }
  const feedVisible = enabled.codex && state.showResetFeed !== false
  const isWorkBuddy = selectedProvider === 'workbuddy'
  const refreshParts = []
  if (isWorkBuddy) {
    const detailsVisible = state.showWorkBuddyHistory !== false
    refreshParts.push(i18n.t("{0}每 {1} 分钟", i18n.t(detailsVisible ? '积分余额与明细' : '积分余额'), minutes.workbuddy))
    if (detailsVisible) refreshParts.push(i18n.t("模型倍率与优惠每 {0} 分钟", minutes.models || 10))
  } else {
    refreshParts.push(i18n.t("本机额度每 {0} 分钟", minutes.codex))
    if (feedVisible) refreshParts.push(i18n.t("公开额度动态每 {0} 分钟", minutes.feed))
  }
  document.getElementById('refresh-summary').textContent = i18n.t("自动刷新：{0}", refreshParts.join(' · '))
  document.getElementById('reset-history').hidden = !feedVisible
  const content = document.querySelector('main')
  const scrollTop = content.scrollTop
  content.classList.toggle('workbuddy-view', isWorkBuddy)
  content.classList.toggle('single-provider', !(enabled.codex && enabled.workbuddy))
  const loading = isWorkBuddy ? creditPeriod === 'models' ? state.models?.loading : state.workbuddy?.loading : state.loading
  refresh.disabled = Boolean(loading)
  refresh.textContent = loading ? i18n.t('读取中…') : i18n.t('刷新')
  document.getElementById('provider-label').textContent = `METERNEST · ${isWorkBuddy ? 'WORKBUDDY' : 'CODEX'}`
  document.getElementById('codex-content').hidden = isWorkBuddy
  document.getElementById('workbuddy-content').hidden = !isWorkBuddy
  document.getElementById('codex-source').hidden = isWorkBuddy || !feedVisible
  document.querySelectorAll('[data-provider]').forEach(tab => {
    tab.hidden = !enabled[tab.dataset.provider]
    const active = !tab.hidden && tab.dataset.provider === selectedProvider
    tab.setAttribute('aria-selected', String(active))
    tab.tabIndex = active ? 0 : -1
  })
  renderUsage(state)
  if (feedVisible) renderRecords(state)
  else { expandedRecords.clear(); document.getElementById('records').replaceChildren(); document.getElementById('feed-status').textContent = '' }
  renderWorkBuddy(state)
  syncAccountCardHeights()
  content.scrollTop = scrollTop
  const compact = isWorkBuddy ? state.showWorkBuddyHistory === false : !feedVisible
  document.body.classList.toggle('compact-view', compact)
  fitCompactContent()
}
function resetProviderView() {
  const content = document.querySelector('main')
  expandedRecords.clear()
  creditPages.valid = 1
  creditPages.expired = 1
  document.getElementById('workbuddy-details').scrollTop = 0
  document.getElementById('model-list').scrollTop = 0
  content.scrollTop = 0
  creditPeriod = 'valid'
}
function selectProvider(provider) {
  if (currentState?.enabledProviders?.[provider] === false) return
  if (provider === selectedProvider) return
  if (provider === 'workbuddy') creditPeriod = 'valid'
  resetProviderView()
  const content = document.querySelector('main')
  selectedProvider = provider
  const revision = ++selectionRevision
  if (currentState) render(currentState)
  content.scrollTop = 0
  api.select(provider).then(state => { if (revision === selectionRevision) render(state) }).catch(() => {})
}
document.querySelectorAll('[data-provider]').forEach(tab => {
  tab.addEventListener('click', () => selectProvider(tab.dataset.provider))
  tab.addEventListener('keydown', event => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
    event.preventDefault()
    const available = Array.from(document.querySelectorAll('[data-provider]')).filter(button => !button.hidden).map(button => button.dataset.provider)
    const provider = event.key === 'Home' ? available[0] : event.key === 'End' ? available.at(-1) : available[(available.indexOf(selectedProvider) + 1) % available.length]
    selectProvider(provider)
    document.getElementById(`tab-${provider}`).focus({ preventScroll: true })
  })
})
document.querySelectorAll('[data-credit-period]').forEach(button => button.addEventListener('click', () => {
  if (currentState?.showWorkBuddyHistory === false) return
  document.getElementById('workbuddy-details').scrollTop = 0
  document.getElementById('model-list').scrollTop = 0
  creditPeriod = button.dataset.creditPeriod
  if (currentState) render(currentState)
}))
document.querySelectorAll('[data-credit-page]').forEach(button => button.addEventListener('click', () => {
  document.getElementById('workbuddy-details').scrollTop = 0
  creditPages[creditPeriod] = Math.max(1, creditPages[creditPeriod] + Number(button.dataset.creditPage))
  if (currentState) renderWorkBuddy(currentState)
}))
const unsubscribe = api.onUpdated(render)
// Opening the W popup always collapses previously expanded Codex messages.
const unsubscribeReset = api.onResetExpanded(() => {
  expandedRecords.clear()
  if (currentState) render(currentState)
})
api.read().then(state => { if (selectionRevision === 0) selectedProvider = state.provider || 'codex'; render(state) }).catch(() => { document.getElementById('usage').textContent = i18n.t('面板暂不可用，请关闭后重试。') })
refresh.addEventListener('click', () => { const revision = selectionRevision; api.refresh(selectedProvider === 'workbuddy' && creditPeriod === 'models' ? 'models' : selectedProvider).then(state => { if (revision === selectionRevision) render(state) }).catch(() => { refresh.disabled = false; refresh.textContent = i18n.t('刷新') }) })
document.querySelectorAll('[data-url]').forEach(button => button.addEventListener('click', () => api.open(button.dataset.url).catch(() => {})))
document.addEventListener('click', event => {
  if (event.target === document.documentElement) api.hide().catch(() => {})
})
// Promotion boundaries update from cached rules without a network request.
const modelClock = setInterval(() => {
  if (!document.hidden && currentState?.showWorkBuddyHistory !== false && selectedProvider === 'workbuddy' && creditPeriod === 'models' && currentState && JSON.stringify(modelValues(currentState)) !== modelFingerprint) renderModels(currentState)
}, 1000)
window.addEventListener('beforeunload', () => { clearInterval(modelClock); unsubscribe(); unsubscribeReset() })

let compactFitFrame
function fitCompactContent() {
  cancelAnimationFrame(compactFitFrame)
  if (!document.body.classList.contains('compact-view')) return
  compactFitFrame = requestAnimationFrame(() => {
    if (!document.body.classList.contains('compact-view')) return
    const id = selectedProvider === 'workbuddy' ? 'workbuddy-content' : 'codex-content'
    const card = document.querySelector(`#${id} > .account-card`)
    const cardBottom = card.getBoundingClientRect().bottom - document.body.getBoundingClientRect().top
    const footer = document.querySelector('footer')
    const footerGap = Number.parseFloat(getComputedStyle(footer).marginTop)
    const height = Math.ceil(cardBottom + footerGap + footer.getBoundingClientRect().height)
    api.fit(Math.max(240, Math.min(744, height))).catch(() => {})
  })
}
window.addEventListener('beforeunload', () => cancelAnimationFrame(compactFitFrame), { once: true })

window.__TAURI__?.event.listen('meternest-locale', event => {
  i18n.set(event.payload)
  if (currentState) render(currentState)
})
