#!/usr/bin/env node
const { execFileSync } = require('node:child_process')
const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const { setDmgLayout } = require('./set-dmg-layout')

if (process.platform !== 'darwin') process.exit(0)

const root = path.resolve(__dirname, '..')
const version = require(path.join(root, 'package.json')).version
const dmgDir = path.join(root, 'src-tauri', 'target', 'release', 'bundle', 'dmg')
const pattern = new RegExp(`^MeterNest_${version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}_.*\\.dmg$`)
const dmg = fs.readdirSync(dmgDir)
  .filter(name => pattern.test(name))
  .map(name => path.join(dmgDir, name))
  .sort()
  .at(-1)

if (!dmg) {
  console.error(`DMG not found for version ${version} in ${dmgDir}`)
  process.exit(1)
}

setDmgLayout(dmg)

const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'meternest-dmg-icon-'))
try {
  const icon = path.join(temporary, 'icon.icns')
  fs.copyFileSync(path.join(root, 'src-tauri', 'icons', 'icon.icns'), icon)
  execFileSync('/usr/bin/sips', ['-i', icon], { stdio: 'ignore' })
  const hex = execFileSync('/usr/bin/xattr', ['-px', 'com.apple.ResourceFork', icon], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 })
  const resource = path.join(temporary, 'icon.rsrc')
  fs.writeFileSync(resource, Buffer.from(hex.replace(/[^0-9a-f]/gi, ''), 'hex'))

  const helper = path.join(temporary, 'set-resource-fork')
  execFileSync('/usr/bin/clang', [path.join(__dirname, 'set-resource-fork.c'), '-o', helper])
  const apply = target => {
    execFileSync(helper, [target, resource])
    execFileSync('/usr/bin/SetFile', ['-a', 'C', target])
    const attributes = execFileSync('/usr/bin/GetFileInfo', ['-a', target], { encoding: 'utf8' }).trim()
    if (!attributes.includes('C')) throw new Error(`custom icon flag missing for ${target}: ${attributes}`)
    console.log(`DMG icon applied: ${target}`)
  }

  apply(dmg)
  const release = path.join(root, 'release', path.basename(dmg))
  fs.mkdirSync(path.dirname(release), { recursive: true })
  fs.copyFileSync(dmg, release)
  apply(release)
} finally {
  fs.rmSync(temporary, { recursive: true, force: true })
}
