// node --test scripts/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { CARGO_LOCK_RE, CARGO_TOML_RE, bump } from './bump-version.mjs'

test('bump: 정식 버전', () => {
  assert.equal(bump('0.1.0', 'patch'), '0.1.1')
  assert.equal(bump('0.1.9', 'minor'), '0.2.0')
  assert.equal(bump('1.2.3', 'major'), '2.0.0')
})

test('bump: 프리릴리스는 그 단계의 정식 버전으로', () => {
  assert.equal(bump('0.2.0-rc.1', 'patch'), '0.2.0')
  assert.equal(bump('0.2.0-rc.1', 'minor'), '0.2.0')
  assert.equal(bump('0.2.1-rc.1', 'minor'), '0.3.0')
  assert.equal(bump('1.0.0-beta', 'major'), '1.0.0')
  assert.equal(bump('1.1.0-beta', 'major'), '2.0.0')
})

test('Cargo.toml: [package] 안에서 version 앞에 "[" 가 든 줄이 있어도 찾는다', () => {
  const toml = '[package]\nname = "band"\nauthors = ["Band Team"]\nversion = "0.1.0"\n\n[dependencies]\nversion = "9"\n'
  assert.equal(toml.match(CARGO_TOML_RE)?.[2], '0.1.0')
  assert.equal(toml.replace(CARGO_TOML_RE, '$10.2.0$3').match(/version = "([^"]+)"/g)?.join(), 'version = "0.2.0",version = "9"')
})

test('Cargo.toml: CRLF', () => {
  const toml = '[package]\r\nname = "band"\r\nversion = "0.1.0"\r\n'
  assert.equal(toml.match(CARGO_TOML_RE)?.[2], '0.1.0')
})

test('Cargo.toml: [package]에 version이 없으면 다음 섹션의 version을 잡지 않는다', () => {
  const toml = '[package]\nname = "band"\n\n[dependencies.foo]\nversion = "9"\n'
  assert.equal(toml.match(CARGO_TOML_RE), null)
})

test('Cargo.lock: band 패키지 version', () => {
  const lock = '[[package]]\nname = "bandwidth"\nversion = "1.0.0"\n\n[[package]]\nname = "band"\nversion = "0.1.0"\n'
  assert.equal(lock.match(CARGO_LOCK_RE)?.[2], '0.1.0')
})
