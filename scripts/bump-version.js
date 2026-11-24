#!/usr/bin/env node
// 与参考项目一致的十进制进位版本递增（minor/patch 限制 0-9）。
const fs = require('node:fs')
const path = require('node:path')
const root = path.resolve(__dirname, '..')
const packageFile = path.join(root, 'package.json')
const lockFile = path.join(root, 'package-lock.json')
const tauriFile = path.join(root, 'src-tauri', 'tauri.conf.json')
const cargoFile = path.join(root, 'src-tauri', 'Cargo.toml')
const read = file => JSON.parse(fs.readFileSync(file, 'utf8'))
const write = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + '\n')
const package = read(packageFile)
const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(String(package.version))
if (!match) throw new Error('version must be major.minor.patch')
let [, major, minor, patch] = match.map(Number)
patch += 1
if (patch > 9) { patch = 0; minor += 1 }
if (minor > 9) { minor = 0; major += 1 }
const version = `${major}.${minor}.${patch}`
const lock = read(lockFile)
const tauri = read(tauriFile)
const cargo = fs.readFileSync(cargoFile, 'utf8')
package.version = version
lock.version = version
lock.packages[''].version = version
tauri.version = version
write(packageFile, package)
write(lockFile, lock)
write(tauriFile, tauri)
fs.writeFileSync(cargoFile, cargo.replace(/^version\s*=\s*"\d+\.\d+\.\d+"/m, `version = "${version}"`))
console.log(`version bumped to ${version}`)
