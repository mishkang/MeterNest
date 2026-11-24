// Hover tooltip renderer: mirrors what the Electron main process injected via
// executeJavaScript. It renders the summary, measures itself and reports the
// exact box size back to Rust, which positions and shows the window.
(() => {
  const { invoke } = window.__TAURI__.core
  const { listen } = window.__TAURI__.event
  const setSummary = value => {
    for (const provider of ['codex', 'workbuddy']) {
      const node = document.getElementById(provider)
      node.closest('.row').hidden = !Object.hasOwn(value, provider)
      node.textContent = value[provider] || ''
    }
    const box = document.body.getBoundingClientRect()
    return { width: Math.ceil(box.width), height: Math.ceil(box.height) }
  }
  const update = value => {
    const size = setSummary(value)
    return invoke('meternest_hover_fit', { width: size.width, height: size.height })
  }
  listen('meternest-hover-updated', event => update(event.payload).catch(() => {}))
  invoke('meternest_hover_ready').catch(() => {})
})()
