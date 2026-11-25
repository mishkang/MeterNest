const $ = id => document.getElementById(id)
const errorMessage = error => i18n.message(typeof error === 'string' ? error : error?.message || '操作未完成，请重试')
let requestedPage
function render(value) {
  i18n.set(value.locale)
  $('language').value = value.language || 'system'
  $('show-reset-feed').checked = value.showResetFeed !== false
  $('show-workbuddy-history').checked = value.showWorkBuddyHistory !== false
  for (const key of ['codex', 'workbuddy']) $(`source-${key}`).checked = value.enabledProviders[key]
  $('startup').checked = value.openAtLogin
  $('startup').disabled = !value.startupSupported
  $('startup-hint').textContent = value.startupSupported ? i18n.t('下次登录时仅启动菜单栏，首次自动查询延迟 10 秒。') : i18n.t('请先安装正式应用，再开启登录启动。')
  setMode(value.networkMode)
  $('proxy').value = value.proxyUrl
  $('proxy').disabled = value.networkMode === 'direct'
  $('tray-value').checked = value.showTrayValue
  document.querySelector(`input[name="tray-source"][value="${value.trayValueSource}"]`).checked = true
  syncSourceControls()
  for (const key of ['codex', 'workbuddy', 'feed', 'models']) $(`refresh-${key}`).value = value.refreshMinutes[key]
  $('version').textContent = `MeterNest ${value.version}`
  $('about-version').textContent = i18n.t("版本 {0}", value.version)
  renderAutomation(value)
  $('save').disabled = false
  selectTab(settingPages[requestedPage || value.initialPage] ? requestedPage || value.initialPage : 'general')
  fitToAutomationPage()
}
const modeOptions = Array.from($('mode-options').querySelectorAll('[role=option]'))
function setMode(value) {
  $('mode').value = value
  $('mode-text').textContent = modeOptions.find(option => option.dataset.value === value).querySelector('span').textContent
  modeOptions.forEach(option => option.setAttribute('aria-selected', String(option.dataset.value === value)))
  $('proxy').disabled = value === 'direct'
}
function closeModes(restoreFocus = false) {
  $('mode-options').hidden = true
  $('mode').setAttribute('aria-expanded', 'false')
  if (restoreFocus) $('mode').focus()
}
function openModes(index = modeOptions.findIndex(option => option.dataset.value === $('mode').value)) {
  $('mode-options').hidden = false
  $('mode').setAttribute('aria-expanded', 'true')
  modeOptions[index].focus()
}
$('mode').addEventListener('click', () => {
  if ($('mode-options').hidden) openModes()
  else closeModes(true)
})
$('mode').addEventListener('change', () => setMode($('mode').value))
modeOptions.forEach(option => option.addEventListener('click', () => {
  setMode(option.dataset.value)
  closeModes(true)
}))
$('mode-select').addEventListener('keydown', event => {
  const index = modeOptions.indexOf(document.activeElement)
  if (event.key === 'Escape') {
    if (!$('mode-options').hidden) { event.preventDefault(); event.stopPropagation(); closeModes(true) }
  } else if (event.key === 'Tab') closeModes()
  else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    if ($('mode-options').hidden) openModes(event.key === 'Home' ? 0 : event.key === 'End' ? modeOptions.length - 1 : undefined)
    else openModes(event.key === 'Home' ? 0 : event.key === 'End' ? modeOptions.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + modeOptions.length) % modeOptions.length)
  } else if (index >= 0 && (event.key === 'Enter' || event.key === ' ')) {
    event.preventDefault()
    modeOptions[index].click()
  }
})
document.addEventListener('pointerdown', event => {
  if (!$('mode-select').contains(event.target)) closeModes()
})
$('mode-select').addEventListener('focusout', event => {
  if (!$('mode-select').contains(event.relatedTarget)) closeModes()
})
function sourceChoices() { return { codex: $('source-codex').checked, workbuddy: $('source-workbuddy').checked } }
function syncSourceControls() {
  const enabled = sourceChoices()
  const radios = Array.from(document.querySelectorAll('input[name="tray-source"]'))
  for (const radio of radios) radio.disabled = !enabled[radio.value]
  if (!radios.some(radio => radio.checked && !radio.disabled)) radios.find(radio => !radio.disabled).checked = true
  $('tray-source').disabled = !$('tray-value').checked
  $('show-reset-feed').disabled = !enabled.codex
  $('show-workbuddy-history').disabled = !enabled.workbuddy
  for (const [key, source] of Object.entries({ codex: 'codex', feed: 'codex', workbuddy: 'workbuddy', models: 'workbuddy' })) $(`refresh-${key}`).disabled = !enabled[source] || key === 'feed' && !$('show-reset-feed').checked || key === 'models' && !$('show-workbuddy-history').checked
}
for (const key of ['codex', 'workbuddy']) $(`source-${key}`).addEventListener('change', () => {
  if (!Object.values(sourceChoices()).some(Boolean)) {
    $(`source-${key}`).checked = true
    $('status').textContent = i18n.t('至少保留一个显示来源')
  } else $('status').textContent = i18n.t('保存后生效')
  syncSourceControls()
  syncAutomationControls()
})
$('tray-value').addEventListener('change', syncSourceControls)
$('show-reset-feed').addEventListener('change', syncSourceControls)
$('show-workbuddy-history').addEventListener('change', syncSourceControls)
function markDirty() { $('status').textContent = i18n.t('有未保存的修改') }
$('settings').addEventListener('input', markDirty)
$('settings').addEventListener('submit', async event => {
  event.preventDefault()
  const refreshMinutes = {}
  for (const key of ['codex', 'workbuddy', 'feed', 'models']) {
    const input = $(`refresh-${key}`)
    const value = input.value.trim()
    if (!/^[0-9]+$/.test(value) || !Number.isSafeInteger(Number(value)) || Number(value) < 1) {
      $('status').textContent = i18n.t('刷新间隔必须是不小于 1 的正整数（分钟）')
      selectTab('network')
      input.focus()
      return
    }
    refreshMinutes[key] = Number(value)
  }
  let automation
  try { automation = automationDraft() } catch (error) { $('status').textContent = errorMessage(error); selectTab('automation'); return }
  $('save').disabled = true
  $('status').textContent = i18n.t('正在保存…')
  try {
    let notificationDenied = false
    if (automation.codex.notify || automation.workbuddy.notify) {
      notificationDenied = !await window.meterNestSettings.authorizeNotifications()
    }
    render(await window.meterNestSettings.save({ language: $('language').value, automation, showWorkBuddyHistory: $('show-workbuddy-history').checked, showResetFeed: $('show-reset-feed').checked, enabledProviders: sourceChoices(), refreshMinutes, openAtLogin: $('startup').checked, networkMode: $('mode').value, proxyUrl: $('proxy').value, showTrayValue: $('tray-value').checked, trayValueSource: document.querySelector('input[name="tray-source"]:checked').value }))
    $('status').textContent = notificationDenied ? i18n.t('已保存，请在系统设置中允许 MeterNest 通知') : i18n.t('已保存，设置已生效')
  } catch (error) { $('status').textContent = errorMessage(error); $('save').disabled = false }
})
window.meterNestSettings.read().then(value => {
  render(value)
}).catch(() => { $('status').textContent = i18n.t('读取设置失败，请关闭后重试') })

const settingPages = { general: ['显示与启动', '选择你关心的内容，让菜单栏保持简洁。'], network: ['刷新与网络', '按你的使用习惯，安排更新频率与连接方式。'], automation: ['自动任务', '预热与签到，交给 MeterNest 按时处理。'], about: ['关于', 'Codex 与 WorkBuddy 的菜单栏额度助手。'] }
const pageKeys = Object.keys(settingPages)
let savedSources = { codex: true, workbuddy: true }, actionState = {}
function selectTab(name) {
  if (!Object.hasOwn(settingPages, name)) return
  requestedPage = name
  closeModes()
  for (const key of pageKeys) {
    const active = key === name
    $(`tab-${key}`).setAttribute('aria-selected', String(active))
    $(`tab-${key}`).tabIndex = active ? 0 : -1
    $(`${key}-settings`).hidden = !active
  }
  document.querySelector('.actions').hidden = name === 'about'
  $('page-title').textContent = i18n.t(settingPages[name][0])
  $('page-description').textContent = i18n.t(settingPages[name][1])
  if (name === 'automation') window.meterNestSettings.automationState().then(renderActionState).catch(() => {})
}
for (const name of pageKeys) {
  $(`tab-${name}`).addEventListener('click', () => selectTab(name))
  $(`tab-${name}`).addEventListener('keydown', event => {
    if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) {
      event.preventDefault()
      const step = ['ArrowLeft', 'ArrowUp'].includes(event.key) ? -1 : 1
      const next = event.key === 'Home' ? pageKeys[0] : event.key === 'End' ? pageKeys.at(-1) : pageKeys[(pageKeys.indexOf(name) + step + pageKeys.length) % pageKeys.length]
      selectTab(next); $(`tab-${next}`).focus()
    }
  })
}
function addTime(time = '09:00') {
  if ($('warmup-times').children.length >= 12) return
  const chip = document.createElement('div'); chip.className = 'time-chip'
  const parts = time.split(':')
  for (let index = 0; index < 2; index++) {
    if (index) chip.append(document.createTextNode(':'))
    const input = document.createElement('input')
    input.type = 'text'; input.inputMode = 'numeric'; input.maxLength = 2; input.value = parts[index]
    input.setAttribute('aria-label', index ? i18n.t('分钟') : i18n.t('小时'))
    const maximum = index ? 59 : 23
    input.addEventListener('focus', () => input.select())
    input.addEventListener('input', () => { input.value = input.value.replace(/[^0-9]/g, '').slice(0, 2) })
    input.addEventListener('blur', () => { if (input.value && Number(input.value) <= maximum) input.value = input.value.padStart(2, '0') })
    input.addEventListener('keydown', event => {
      if (['ArrowUp', 'ArrowDown'].includes(event.key)) {
        event.preventDefault(); input.value = String((Number(input.value || 0) + (event.key === 'ArrowUp' ? 1 : -1) + maximum + 1) % (maximum + 1)).padStart(2, '0'); input.select()
      } else if (event.key === 'Enter') { event.preventDefault(); input.blur() }
    })
    chip.append(input)
  }
  const remove = document.createElement('button'); remove.type = 'button'; remove.textContent = '×'; remove.setAttribute('aria-label', i18n.t('移除此时间'))
  remove.addEventListener('click', () => { chip.remove(); syncAutomationControls(); markDirty() })
  chip.append(remove); $('warmup-times').append(chip)
}
let wakeState = {}, wakeBusy = false
function renderWakeState(value) {
  wakeState = value
  $('manage-wake').textContent = value.installed ? i18n.t('移除组件') : i18n.t('授权安装')
  $('manage-wake').disabled = wakeBusy || !value.available
  $('wake-status').textContent = i18n.message(value.error) || (!value.available ? i18n.t('正式安装的应用中可授权使用') : !value.installed ? i18n.t('尚未授权；仅在点击授权时请求管理员权限') : value.outcome === 'error' ? i18n.t('系统未能登记唤醒计划，请检查电源设置') : value.scheduledAt ? i18n.t('已安排唤醒 {0}', new Date(value.scheduledAt).toLocaleString(i18n.language, { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })) : i18n.t('组件已安装；启用开关并保存后生效'))
}
$('manage-wake').addEventListener('click', async () => {
  if (wakeBusy) return
  const action = wakeState.installed ? 'uninstall' : 'install'
  wakeBusy = true; renderWakeState(wakeState)
  try { const value = await window.meterNestSettings.manageWake(action); if (action === 'uninstall') $('codex-wake').checked = false; renderWakeState(value); syncAutomationControls() }
  catch (error) { $('wake-status').textContent = errorMessage(error) }
  finally { wakeBusy = false; $('manage-wake').disabled = !wakeState.available }
})
function automationDraft() {
  const inputs = Array.from($('warmup-times').querySelectorAll('input'))
  if (inputs.some(input => !/^[0-9]{1,2}$/.test(input.value))) throw new Error(i18n.t('请填写完整的小时和分钟。'))
  const times = Array.from($('warmup-times').children, chip => Array.from(chip.querySelectorAll('input'), input => input.value.padStart(2, '0')).join(':'))
  if (times.some(time => !/^(?:[01]\d|2[0-3]):[0-5]\d$/.test(time)) || new Set(times).size !== times.length || $('codex-daily').checked && !times.length) throw new Error(i18n.t('请填写有效、不重复的时间；每日定时至少保留一个时间。'))
  return { codex: { notify: $('codex-notify').checked, wake: $('codex-wake').checked, onReset: $('codex-reset').checked, daily: $('codex-daily').checked, times }, workbuddy: { notify: $('workbuddy-notify').checked, daily: $('workbuddy-daily').checked } }
}
function renderAutomation(value) {
  savedSources = value.enabledProviders
  const automation = value.automation || { codex: { times: ['06:00', '11:02', '16:05'] }, workbuddy: {} }
  $('codex-notify').checked = Boolean(automation.codex.notify)
  $('workbuddy-notify').checked = Boolean(automation.workbuddy.notify)
  $('codex-wake').checked = Boolean(automation.codex.wake)
  renderWakeState(value.wakeState || {})
  $('codex-reset').checked = Boolean(automation.codex.onReset) && value.automationState?.codex?.windowSupported !== false
  $('codex-daily').checked = Boolean(automation.codex.daily)
  $('workbuddy-daily').checked = Boolean(automation.workbuddy.daily)
  $('warmup-times').replaceChildren(); automation.codex.times.forEach(addTime)
  renderActionState(value.automationState || actionState)
}
function syncAutomationControls() {
  const sources = sourceChoices()
  const windowSupported = actionState.codex?.windowSupported !== false
  $('codex-reset').disabled = !sources.codex || !windowSupported
  if (!windowSupported) $('codex-reset').checked = false
  $('codex-daily').disabled = !sources.codex
  const wakeUsable = windowSupported || $('codex-daily').checked
  if (!wakeUsable) $('codex-wake').checked = false
  $('codex-wake').disabled = !sources.codex || !wakeUsable || !wakeState.installed && !$('codex-wake').checked
  $('workbuddy-daily').disabled = !sources.workbuddy
  for (const el of $('warmup-times').querySelectorAll('input,button')) el.disabled = !sources.codex
  $('add-time').disabled = !sources.codex || $('warmup-times').children.length >= 12
  for (const p of ['codex', 'workbuddy']) $(`run-${p}`).disabled = !savedSources[p] || !sources[p] || Boolean(actionState[p]?.running)
}
const outcomes = { idle: '尚未执行', error: '执行失败，请重试', pending: '今日尚未签到', confirmed: '预热成功', sent: '预热请求已完成', skipped: '当前窗口已开始，已跳过预热', claimed: '签到成功', done: '今天已经签到', inactive: '当前没有可参加的签到活动', ineligible: '当前账号不符合签到条件', 'own-app': '请在 WorkBuddy 客户端完成签到', uncertain: '请求结果未确认，本次不自动重试' }
function windowSupportMessage(state) {
  const minutes = state.windowDurationMinutes
  if (!minutes) return i18n.t('未读取到 5 小时或每周额度窗口，当前套餐不支持窗口到期预热。')
  const [unit, size] = [['{0} 周', 10080], ['{0} 天', 1440], ['{0} 小时', 60], ['{0} 分钟', 1]]
    .find(([, divisor]) => minutes % divisor === 0) || ['{0} 分钟', 1]
  return i18n.t('当前套餐仅提供 {0} 额度窗口，不支持窗口到期预热。', i18n.t(unit, minutes / size))
}

function renderActionState(value) {
  actionState = value
  if (value.wake) renderWakeState(value.wake)
  for (const p of ['codex', 'workbuddy']) {
    const s = value[p] || {}
    const unsupported = p === 'codex' && s.windowSupported === false
    let text = s.running ? (p === 'codex' ? i18n.t('正在预热…') : i18n.t('正在签到…')) : s.checking ? i18n.t('正在检查…') : s.paused ? i18n.t('来源已隐藏，任务暂停') : i18n.message(s.message || outcomes[s.outcome] || '尚未执行')
    if (s.outcome === 'error' && !s.running && !s.checking && !s.paused) {
      if (s.retryAt > Date.now()) text += i18n.t(" · 将于 {0} 重试", new Date(s.retryAt).toLocaleTimeString(i18n.language, { hour: '2-digit', minute: '2-digit', hour12: false }))
      else if (s.retryStopped) text += i18n.t(' · 已达到自动重试次数上限')
    }
    if (!s.running && !s.paused && !s.message && ['claimed', 'done'].includes(s.outcome) && Number.isFinite(s.credit)) text += i18n.t(" · +{0} 积分", s.credit)
    if (p === 'codex') {
      $('codex-window-hint').textContent = unsupported
        ? windowSupportMessage(s)
        : i18n.t('任一窗口到期检查；周恢复不受 5 小时窗口限制')
      const formatTime = (time, empty) => Number.isFinite(time) && time > 0
        ? new Date(time).toLocaleString(i18n.language, { timeZone: 'Asia/Shanghai', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })
        : empty
      $('codex-previous-recovery').textContent = formatTime(s.previousWarmupCompletedAt, i18n.t('无历史记录'))
      $('codex-last-success').textContent = formatTime(s.warmupCompletedAt, i18n.t('尚无完成记录'))
      $('codex-next-recovery').textContent = formatTime(s.nextQuotaRecoveryAt, i18n.t('待额度更新'))
    } else if (!s.running && s.at && !s.paused) text += ` · ${s.trigger === 'status' ? i18n.t('查询于 ') : ''}${new Date(s.at).toLocaleString(i18n.language, { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false })}`
    $(`${p}-action-status`).textContent = text
  }
  syncAutomationControls()
}
$('add-time').addEventListener('click', () => {
  const existing = new Set(Array.from($('warmup-times').children, chip => Array.from(chip.querySelectorAll('input'), input => input.value.padStart(2, '0')).join(':')))
  for (let hour = 0; hour < 24; hour++) { const time = String((9 + hour) % 24).padStart(2, '0') + ':00'; if (!existing.has(time)) { addTime(time); break } }
  syncAutomationControls(); markDirty()
})
for (const p of ['codex', 'workbuddy']) $(`run-${p}`).addEventListener('click', async () => {
  actionState[p] = { ...actionState[p], running: true }; renderActionState(actionState)
  try { renderActionState(await window.meterNestSettings.runAutomation(p)) }
  catch { actionState[p] = { running: false, message: i18n.t('操作未完成，请检查来源设置与登录状态后重试。') }; renderActionState(actionState) }
})
const unsubscribeNavigate = window.meterNestSettings.onNavigate(selectTab)
window.addEventListener('beforeunload', unsubscribeNavigate, { once: true })
const unsubscribeAutomation = window.meterNestSettings.onAutomationUpdated(renderActionState)
window.addEventListener('beforeunload', unsubscribeAutomation, { once: true })

function fitToAutomationPage() {
  const page = $('automation-settings')
  const actions = document.querySelector('.actions')
  const pageWasHidden = page.hidden
  const actionsWereHidden = actions.hidden
  const previousZoom = page.style.zoom
  page.hidden = false
  page.style.zoom = '1'
  actions.hidden = false
  const numberStyle = (element, property) => parseFloat(getComputedStyle(element)[property]) || 0
  const outerHeight = element => element.getBoundingClientRect().height
    + numberStyle(element, 'marginTop') + numberStyle(element, 'marginBottom')
  const main = document.querySelector('.settings-main')
  const sections = document.querySelector('.settings-sections')
  const height = [main, sections].reduce((total, element) =>
    total + numberStyle(element, 'paddingTop') + numberStyle(element, 'paddingBottom'), 0)
    + [document.querySelector('header'), page, actions, document.querySelector('footer')]
      .reduce((total, element) => total + outerHeight(element), 0)
  page.hidden = pageWasHidden
  page.style.zoom = previousZoom
  actions.hidden = actionsWereHidden
  window.meterNestSettings.fit(Math.ceil(height) + 2).catch(() => {})
}

const settingsSections = document.querySelector('.settings-sections')
const settingsLayout = new ResizeObserver(() => {
  const page = settingsSections.querySelector('[role=tabpanel]:not([hidden])')
  if (!page) return
  page.style.zoom = '1'
  const style = getComputedStyle(settingsSections)
  const available = settingsSections.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom)
  const content = Math.max(page.scrollHeight, page.getBoundingClientRect().height)
  if (available > 0 && content > available) page.style.zoom = String(available / content)
})
settingsLayout.observe(settingsSections)
for (const page of settingsSections.querySelectorAll('[role=tabpanel]')) settingsLayout.observe(page)
settingsLayout.observe($('mode-options'))
window.addEventListener('beforeunload', () => settingsLayout.disconnect(), { once: true })
