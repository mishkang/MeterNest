const i18n = (() => {
  let locale = window.meterNestLocale || {language: 'zh-Hans', strings: {}}
  const staticText = []
  const walker = document.createTreeWalker(document.documentElement, NodeFilter.SHOW_TEXT)
  while (walker.nextNode()) {
    const node = walker.currentNode
    if (node.parentElement.closest('script, style, [data-language-name]') || !/[\u3400-\u9fff]/u.test(node.data)) continue
    const source = node.data
    staticText.push(() => { node.data = source.replace(/\S(?:.*\S)?/su, text => t(text)) })
  }
  for (const node of document.querySelectorAll('[aria-label], [title]')) {
    for (const attribute of ['aria-label', 'title']) {
      const source = node.getAttribute(attribute)
      if (source) staticText.push(() => node.setAttribute(attribute, t(source)))
    }
  }

  function t(key, ...values) {
    if (typeof key !== 'string') return key
    const translated = Object.hasOwn(locale.strings, key) ? locale.strings[key] : key
    return values.length ? translated.replace(/\{(\d+)\}/g, (match, index) => values[index] ?? match) : translated
  }

  function message(source) {
    if (!source || typeof source !== 'string' || locale.language === 'zh-Hans') return source
    const cached = source.match(/^(.*?) (显示上次成功结果，.*。)$/u)
    if (cached) return `${message(cached[1])} ${t(cached[2])}`
    const response = source.match(/^(网站拒绝访问|网站响应异常)（HTTP ([^)）]+)）$/u)
    if (Object.hasOwn(locale.strings, source)) return t(source)
    if (response) return t(`${response[1]}（HTTP {0}）`, t(response[2]))
    return source
  }

  function set(value) {
    if (value) locale = value
    document.documentElement.lang = locale.language
    staticText.forEach(update => update())
  }

  set(locale)
  return {t, message, set, get language() { return locale.language }}
})()
