// Tauri IPC bridge exposing the same window.codexMonitor API as the Electron preload.
(() => {
  const { invoke } = window.__TAURI__.core
  const { listen } = window.__TAURI__.event
  const tracked = []
  function onEvent(name, callback) {
    let unlisten
    listen(name, event => callback(event.payload)).then(stop => { unlisten = stop })
    return () => { try { unlisten && unlisten() } catch {} }
  }
  window.codexMonitor = {
    fit: (provider, height) => invoke('codex_monitor_fit', { provider, height }),
    read: () => invoke('codex_monitor_read'),
    select: provider => invoke('codex_monitor_select', { provider }),
    refresh: provider => invoke('codex_monitor_refresh', { provider }),
    open: url => invoke('codex_monitor_open', { url }),
    hide: () => invoke('codex_monitor_hide'),
    onUpdated: callback => onEvent('codex-monitor-updated', callback),
    onResetExpanded: callback => onEvent('codex-monitor-reset-expanded', callback)
  }
  window.addEventListener('beforeunload', () => { for (const stop of tracked) stop() })
})()
