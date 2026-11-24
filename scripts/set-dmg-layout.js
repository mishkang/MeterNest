const { execFileSync } = require('node:child_process')
const fs = require('node:fs')
const path = require('node:path')

// Write Finder preferences directly so packaging never opens a Finder window.
function setDmgLayout(dmg) {
  const template = path.join(__dirname, 'assets', 'dmg-layout.dsstore')
  if (!fs.statSync(template).isFile()) throw new Error('Missing DMG layout template')
  const staging = fs.mkdtempSync(path.join(path.dirname(dmg), '.meternest-dmg-layout-'))
  const writable = path.join(staging, 'writable.dmg')
  const output = path.join(staging, 'formatted.dmg')
  const mount = path.join(staging, 'mount')
  let attached = false
  const hdiutil = args => execFileSync('/usr/bin/hdiutil', args, { stdio: 'inherit' })
  try {
    hdiutil(['convert', dmg, '-format', 'UDRW', '-o', writable])
    fs.mkdirSync(mount)
    hdiutil(['attach', writable, '-readwrite', '-noverify', '-noautoopen', '-nobrowse', '-mountpoint', mount])
    attached = true
    for (const entry of ['MeterNest.app', 'Applications']) {
      if (!fs.existsSync(path.join(mount, entry))) throw new Error(`Missing DMG entry: ${entry}`)
    }
    fs.copyFileSync(template, path.join(mount, '.DS_Store'))
    fs.chmodSync(path.join(mount, '.DS_Store'), 0o644)
    // FinderInfo on a signed app bundle makes strict signature verification fail.
    const app = path.join(mount, 'MeterNest.app')
    const attributes = execFileSync('/usr/bin/xattr', [app], { encoding: 'utf8' }).split('\n')
    if (attributes.includes('com.apple.FinderInfo')) {
      execFileSync('/usr/bin/xattr', ['-d', 'com.apple.FinderInfo', app])
    }
    execFileSync('/usr/bin/codesign', ['--verify', '--deep', '--strict', app], { stdio: 'inherit' })
    hdiutil(['detach', mount])
    attached = false
    hdiutil(['convert', writable, '-format', 'UDZO', '-o', output])
    fs.renameSync(output, dmg)
    console.log(`DMG layout applied: ${dmg}`)
  } finally {
    if (attached) hdiutil(['detach', mount])
    fs.rmSync(staging, { recursive: true, force: true })
  }
}

module.exports = { setDmgLayout }
