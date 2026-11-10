#!/usr/bin/env node
/**
 * 데스크톱 앱 버전 단일 출처 동기화 (P2-1).
 *
 * 기준: client/src-tauri/tauri.conf.json `version` (업데이터가 비교하는 값)
 * 동기화 대상: client/src-tauri/Cargo.toml [package] version, client/src-tauri/Cargo.lock(band),
 *             client/package.json, package-lock.json(client 워크스페이스)
 *
 * 사용:
 *   node scripts/bump-version.mjs 0.2.0     # 모든 파일을 0.2.0으로
 *   node scripts/bump-version.mjs patch     # major|minor|patch 증가
 *   node scripts/bump-version.mjs --check   # 모두 같은지 확인 (다르면 exit 1)
 *   node scripts/bump-version.mjs --check 0.2.0   # 모두 0.2.0인지 확인 (릴리스 태그 검증)
 */
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const P = {
  tauriConf: join(root, 'client/src-tauri/tauri.conf.json'),
  cargoToml: join(root, 'client/src-tauri/Cargo.toml'),
  cargoLock: join(root, 'client/src-tauri/Cargo.lock'),
  clientPkg: join(root, 'client/package.json'),
  rootLock: join(root, 'package-lock.json'),
}
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/

const read = (p) => readFileSync(p, 'utf8')
/** 원래 줄바꿈(CRLF/LF)을 유지해서 쓴다 */
function write(p, text, original) {
  const eol = original.includes('\r\n') ? '\r\n' : '\n'
  writeFileSync(p, text.replace(/\r?\n/g, eol))
}

// Cargo.toml: [package] 섹션의 첫 version 줄. 다음 섹션 헤더(줄 맨 앞 '[')를 넘어가지 않고,
// 'authors = [...]'처럼 줄 중간의 '['는 허용한다.
export const CARGO_TOML_RE = /(^\[package\][ \t]*\r?\n(?:(?!\[|version\s*=)[^\n]*\n)*?version\s*=\s*")([^"]+)(")/m
// Cargo.lock: name = "band" 바로 다음 version 줄
export const CARGO_LOCK_RE = /(\nname = "band"\r?\nversion = ")([^"]+)(")/

function current() {
  const tauri = JSON.parse(read(P.tauriConf)).version
  const cargo = read(P.cargoToml).match(CARGO_TOML_RE)?.[2]
  const lock = read(P.cargoLock).match(CARGO_LOCK_RE)?.[2]
  const pkg = JSON.parse(read(P.clientPkg)).version
  const rootLock = JSON.parse(read(P.rootLock)).packages?.client?.version
  return {
    'tauri.conf.json': tauri,
    'Cargo.toml': cargo,
    'Cargo.lock': lock,
    'client/package.json': pkg,
    'package-lock.json (client)': rootLock,
  }
}

function setVersion(v) {
  const conf = read(P.tauriConf)
  const confJson = JSON.parse(conf)
  confJson.version = v
  write(P.tauriConf, JSON.stringify(confJson, null, 2) + '\n', conf)

  const toml = read(P.cargoToml)
  if (!CARGO_TOML_RE.test(toml)) throw new Error('Cargo.toml [package] version을 찾지 못했습니다')
  write(P.cargoToml, toml.replace(CARGO_TOML_RE, `$1${v}$3`), toml)

  const lock = read(P.cargoLock)
  if (!CARGO_LOCK_RE.test(lock)) throw new Error('Cargo.lock의 band 패키지를 찾지 못했습니다')
  write(P.cargoLock, lock.replace(CARGO_LOCK_RE, `$1${v}$3`), lock)

  const pkg = read(P.clientPkg)
  const pkgJson = JSON.parse(pkg)
  pkgJson.version = v
  write(P.clientPkg, JSON.stringify(pkgJson, null, 2) + '\n', pkg)

  const rl = read(P.rootLock)
  const rlJson = JSON.parse(rl)
  if (rlJson.packages?.client) {
    rlJson.packages.client.version = v
    write(P.rootLock, JSON.stringify(rlJson, null, 2) + '\n', rl)
  }
}

/** semver 증가. 프리릴리스(x.y.z-rc.1)는 그 단계의 정식 버전으로 올린다 (0.2.0-rc.1 patch → 0.2.0). */
export function bump(v, part) {
  const dash = v.indexOf('-')
  const pre = dash >= 0
  const [maj, min, pat] = (pre ? v.slice(0, dash) : v).split('.').map(Number)
  if (part === 'major') return pre && min === 0 && pat === 0 ? `${maj}.0.0` : `${maj + 1}.0.0`
  if (part === 'minor') return pre && pat === 0 ? `${maj}.${min}.0` : `${maj}.${min + 1}.0`
  return pre ? `${maj}.${min}.${pat}` : `${maj}.${min}.${pat + 1}`
}

function main(argv) {
  const cur = current()
  if (argv[0] === '--check') {
    const want = argv[1]?.replace(/^v/, '')
    const values = Object.values(cur)
    const ok = values.every((x) => x !== undefined && x === (want ?? values[0]))
    for (const [k, x] of Object.entries(cur)) console.log(`${k.padEnd(28)} ${x ?? '(버전 줄을 찾지 못함)'}`)
    if (!ok) {
      console.error(want ? `버전이 ${want}와(과) 일치하지 않습니다.` : '버전이 서로 다릅니다.')
      console.error('`node scripts/bump-version.mjs <버전>`으로 맞추세요.')
      process.exit(1)
    }
    console.log('OK')
    return
  }

  const arg = argv[0]
  if (!arg) {
    console.error('사용: node scripts/bump-version.mjs <x.y.z|major|minor|patch|--check [x.y.z]>')
    process.exit(2)
  }
  const next = ['major', 'minor', 'patch'].includes(arg) ? bump(cur['tauri.conf.json'], arg) : arg.replace(/^v/, '')
  if (!SEMVER.test(next)) {
    console.error(`잘못된 버전: ${arg}`)
    process.exit(2)
  }
  setVersion(next)
  console.log(`버전 ${cur['tauri.conf.json']} → ${next}`)
  console.log(`다음: git commit -am "chore: v${next}" && git tag v${next} && git push origin v${next}`)
}

// 테스트에서 import할 때는 실행하지 않는다
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) main(process.argv.slice(2))
