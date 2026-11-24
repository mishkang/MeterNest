// Read installed client SVG components as static data. Never evaluate its code.
const fs = require('node:fs/promises')
const path = require('node:path')
const { parseExpressionAt } = require('acorn')
const TAGS = new Set(['svg', 'g', 'path', 'rect', 'circle', 'ellipse', 'line', 'polyline', 'polygon', 'defs', 'clipPath', 'mask'])
const ATTRS = new Set(['xmlns', 'width', 'height', 'viewBox', 'fill', 'fillRule', 'fillOpacity', 'clipRule', 'clipPath', 'stroke', 'strokeWidth', 'strokeLinecap', 'strokeLinejoin', 'strokeOpacity', 'strokeDasharray', 'd', 'points', 'transform', 'id', 'x', 'y', 'x1', 'x2', 'y1', 'y2', 'cx', 'cy', 'r', 'rx', 'ry', 'opacity', 'mask', 'maskUnits', 'clipPathUnits'])
const escape = value => String(value).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;')
function firstExpression(ast) { return ast.type === 'SequenceExpression' ? ast.expressions[0] : ast }
function clientIconReader(source) {
  const anchor = source.indexOf('"fast-model":')
  const start = source.lastIndexOf('={', anchor)
  if (anchor < 0 || start < 0 || anchor - start > 20000) throw new Error('client icon catalog unavailable')
  const object = firstExpression(parseExpressionAt(source, start + 1, { ecmaVersion: 'latest' }))
  if (object.type !== 'ObjectExpression') throw new Error('invalid icon catalog')
  const catalog = new Map()
  for (const item of object.properties) {
    const key = item.key?.name || item.key?.value
    const component = item.value?.arguments?.[0]
    if (typeof key === 'string' && component?.type === 'Identifier') catalog.set(key, component.name)
  }
  const cache = new Map()
  function component(name, depth) {
    if (depth > 6) throw new Error('nested icon limit')
    if (cache.has(name)) return cache.get(name)
    const at = source.indexOf(`${name}=`)
    if (at < 0) throw new Error('missing icon component')
    const arrow = firstExpression(parseExpressionAt(source, at + name.length + 1, { ecmaVersion: 'latest' }))
    if (arrow.type !== 'ArrowFunctionExpression') throw new Error('unsupported icon component')
    const result = value(arrow.body, depth + 1)
    cache.set(name, result)
    return result
  }
  function value(ast, depth = 0) {
    if (!ast || depth > 40) throw new Error('invalid SVG expression')
    if (ast.type === 'Literal') return ast.value
    if (ast.type === 'TemplateLiteral' && !ast.expressions.length) return ast.quasis[0].value.cooked
    if (ast.type === 'UnaryExpression' && ['-', '+'].includes(ast.operator) && typeof ast.argument.value === 'number') return ast.operator === '-' ? -ast.argument.value : ast.argument.value
    if (ast.type === 'ArrayExpression') return ast.elements.map(item => value(item, depth + 1))
    if (ast.type === 'ObjectExpression') {
      const result = Object.create(null)
      for (const item of ast.properties) {
        // Client components spread their caller's SVG props; we supply none.
        if (item.type === 'SpreadElement' && item.argument.type === 'Identifier') continue
        if (item.type !== 'Property' || item.computed || item.kind !== 'init') throw new Error('dynamic SVG property')
        const key = item.key.name || item.key.value
        if (key === 'children' || ATTRS.has(key)) result[key] = value(item.value, depth + 1)
      }
      return result
    }
    if (ast.type === 'CallExpression') {
      const callee = ast.callee.type === 'SequenceExpression' ? ast.callee.expressions.at(-1) : ast.callee
      if (callee.type !== 'MemberExpression' || !['jsx', 'jsxs'].includes(callee.property.name)) throw new Error('executable SVG expression rejected')
      const tag = ast.arguments[0]
      if (tag.type === 'Identifier') return component(tag.name, depth + 1)
      const name = value(tag, depth + 1)
      if (!TAGS.has(name)) throw new Error('unsupported SVG element')
      return { tag: name, props: value(ast.arguments[1], depth + 1) }
    }
    throw new Error('nonliteral SVG data rejected')
  }
  function serialize(node) {
    if (Array.isArray(node)) return node.map(serialize).join('')
    if (!node || !TAGS.has(node.tag)) throw new Error('invalid SVG tree')
    const { children, ...props } = node.props
    if (node.tag === 'svg' && !props.xmlns) props.xmlns = 'http://www.w3.org/2000/svg'
    const attrs = Object.entries(props).map(([key, entry]) => {
      if (typeof entry !== 'string' && typeof entry !== 'number') throw new Error('invalid SVG attribute')
      if (/url\(/i.test(String(entry)) && !/^url\(#[\w-]+\)$/.test(String(entry))) throw new Error('external SVG reference rejected')
      const name = key === 'viewBox' ? key : key.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`)
      return `${name}="${escape(entry)}"`
    }).join(' ')
    return `<${node.tag} ${attrs}>${children ? serialize(children) : ''}</${node.tag}>`
  }
  return model => {
    const key = `${model.id} ${model.name}`.toLowerCase()
    const exact = [model.id, model.name].map(item => String(item).toLowerCase()).find(item => catalog.has(item))
    const brand = exact || [...catalog.keys()].find(item => !['fast', 'balanced', 'primary', 'deep', 'auto', 'auto-chat', 'default-model', 'default'].includes(item) && (key.includes(item) || item === 'openai' && key.includes('gpt'))) || 'default'
    const name = catalog.get(brand)
    if (!name) return null
    const svg = serialize(component(name, 0))
    if (!svg.startsWith('<svg ') || Buffer.byteLength(svg) > 65536) return null
    return { iconUrl: `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}`, iconTemplate: svg.includes('currentColor') }
  }
}
async function attachClientIcons(root, models, io = fs) {
  const missing = models.filter(model => !model.iconUrl)
  if (!missing.length) return models
  try {
    const directory = path.join(root, 'renderer', 'assets')
    const names = (await io.readdir(directory)).filter(name => /^contexts[-\w]*\.js$/.test(name)).slice(0, 8)
    for (const name of names) {
      const file = path.join(directory, name)
      const stat = await io.stat(file)
      if (stat.size > 20 * 1024 * 1024) continue
      const source = await io.readFile(file, 'utf8')
      let read
      try { read = clientIconReader(source) } catch { continue }
      for (const model of missing) {
        try { Object.assign(model, read(model)) } catch { /* Keep the generic fallback if a component changes syntax. */ }
      }
      break
    }
  } catch { /* Client layout changes do not invalidate names, rates or model order. */ }
  return models
}
module.exports = { attachClientIcons, clientIconReader }
