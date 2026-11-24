// Tauri IPC bridge exposing the same window.meterNestSettings API as the Electron preload.
(() => {
  const { invoke } = window.__TAURI__.core
  const { listen } = window.__TAURI__.event
  function onEvent(name, callback) {
    let unlisten
    listen(name, event => callback(event.payload)).then(stop => { unlisten = stop })
    return () => { try { unlisten && unlisten() } catch {} }
  }
  window.meterNestSettings = {
    manageWake: action => invoke('meternest_wake_manage', { action }),
    fit: height => invoke('meternest_settings_fit', { height }),
    authorizeNotifications: () => invoke('meternest_notification_authorize'),
    onNavigate: callback => onEvent('meternest-settings-navigate', callback),
    automationState: () => invoke('meternest_automation_state'),
    runAutomation: source => invoke('meternest_automation_run', { source }),
    onAutomationUpdated: callback => onEvent('meternest-automation-updated', callback),
    read: () => invoke('meternest_settings_read'),
    save: value => invoke('meternest_settings_save', { value })
  }
})()
