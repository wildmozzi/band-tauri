/**
 * C1 회귀 테스트: /api/music/file/:filename 경로 순회 + 인증 + 소유자 검사
 * 재현: GET /api/music/file/..%2F..%2Fsecret → Express 5가 %2F를 디코딩해 업로드 디렉터리 밖 파일을 읽던 문제
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import * as fs from 'fs'
import * as os from 'os'
import * as path from 'path'
import type { Server } from 'http'
import { bearer, testVerifier } from './helpers'

const base = fs.mkdtempSync(path.join(os.tmpdir(), 'band-c1-'))
const uploadDir = path.join(base, 'a', 'uploads')
fs.mkdirSync(uploadDir, { recursive: true })
const SECRET = 'SUPABASE_SERVICE_KEY=leaked'
fs.writeFileSync(path.join(base, 'secret.txt'), SECRET) // 업로드 디렉터리의 두 단계 위
fs.writeFileSync(path.join(base, 'a', 'victim.txt'), 'do not delete')
process.env.UPLOAD_DIR = uploadDir

let server: Server
let url = ''
let uploads: typeof import('../src/lib/uploads')

before(async () => {
  const auth = await import('../src/lib/auth')
  auth.setTokenVerifier(testVerifier)
  uploads = await import('../src/lib/uploads')
  const { createApp } = await import('../src/app')
  server = createApp().listen(0)
  await new Promise(r => server.once('listening', r))
  const addr = server.address() as { port: number }
  url = `http://127.0.0.1:${addr.port}`

  fs.writeFileSync(path.join(uploadDir, '123_score.xml'), '<score/>')
  uploads.recordUploadOwner('123_score.xml', 'alice')
})

after(() => {
  server?.close()
  fs.rmSync(base, { recursive: true, force: true })
})

describe('resolveUploadPath (whitelist)', () => {
  it('rejects traversal, separators, dotfiles and non-strings', () => {
    for (const bad of ['../secret.txt', '..', '.owners.json', 'a/b.xml', 'a\\b.xml', '..%2Fx', '', 'x'.repeat(300), '/etc/passwd', 'C:\\x', 'a..b', null, 1]) {
      assert.equal(uploads.resolveUploadPath(bad), null, `should reject ${String(bad)}`)
    }
  })
  it('accepts plain names inside the upload dir', () => {
    const p = uploads.resolveUploadPath('123_score.xml')
    assert.equal(p, path.join(uploads.UPLOAD_DIR, '123_score.xml'))
  })
})

describe('GET /api/music/file/:filename', () => {
  it('requires authentication', async () => {
    const r = await fetch(`${url}/api/music/file/123_score.xml`)
    assert.equal(r.status, 401)
  })

  it('does not leak files outside the upload dir (encoded traversal)', async () => {
    for (const p of ['..%2F..%2Fsecret.txt', '..%5C..%5Csecret.txt', '%2E%2E%2F%2E%2E%2Fsecret.txt']) {
      const r = await fetch(`${url}/api/music/file/${p}`, { headers: bearer('alice') })
      const body = await r.text()
      assert.notEqual(r.status, 200, `status for ${p}`)
      assert.ok(!body.includes(SECRET), `leaked secret via ${p}`)
    }
  })

  it('rejects the owner registry dotfile', async () => {
    const r = await fetch(`${url}/api/music/file/.owners.json`, { headers: bearer('alice') })
    assert.equal(r.status, 400)
  })

  it('forbids non-owners', async () => {
    const r = await fetch(`${url}/api/music/file/123_score.xml`, { headers: bearer('mallory') })
    assert.equal(r.status, 403)
  })

  it('serves the file to its owner', async () => {
    const r = await fetch(`${url}/api/music/file/123_score.xml`, { headers: bearer('alice') })
    assert.equal(r.status, 200)
    assert.equal(await r.text(), '<score/>')
  })
})

describe('DELETE /api/music/file/:filename', () => {
  it('requires authentication', async () => {
    const r = await fetch(`${url}/api/music/file/123_score.xml`, { method: 'DELETE' })
    assert.equal(r.status, 401)
    assert.ok(fs.existsSync(path.join(uploadDir, '123_score.xml')))
  })

  it('cannot delete files outside the upload dir', async () => {
    const r = await fetch(`${url}/api/music/file/..%2Fvictim.txt`, { method: 'DELETE', headers: bearer('alice') })
    assert.notEqual(r.status, 200)
    assert.ok(fs.existsSync(path.join(base, 'a', 'victim.txt')))
  })

  it('forbids non-owners', async () => {
    const r = await fetch(`${url}/api/music/file/123_score.xml`, { method: 'DELETE', headers: bearer('mallory') })
    assert.equal(r.status, 403)
    assert.ok(fs.existsSync(path.join(uploadDir, '123_score.xml')))
  })

  it('lets the owner delete', async () => {
    const r = await fetch(`${url}/api/music/file/123_score.xml`, { method: 'DELETE', headers: bearer('alice') })
    assert.equal(r.status, 200)
    assert.ok(!fs.existsSync(path.join(uploadDir, '123_score.xml')))
  })
})

describe('uploads (M6)', () => {
  it('rejects anonymous uploads before storing the file', async () => {
    const before = fs.readdirSync(uploadDir).length
    const fd = new FormData()
    fd.append('file', new Blob(['<score/>']), 'x.xml')
    const r = await fetch(`${url}/api/music/upload-score`, { method: 'POST', body: fd })
    assert.equal(r.status, 401)
    assert.equal(fs.readdirSync(uploadDir).length, before)
  })

  it('rejects files whose content does not match the extension', async () => {
    const fd = new FormData()
    fd.append('file', new Blob(['MZ not a midi']), 'evil.mid')
    const r = await fetch(`${url}/api/music/upload-score`, { method: 'POST', body: fd, headers: bearer('alice') })
    assert.equal(r.status, 400)
    assert.ok(!fs.readdirSync(uploadDir).some(f => f.endsWith('evil.mid')))
  })

  it('accepts a valid upload and records the owner', async () => {
    const fd = new FormData()
    fd.append('file', new Blob(['<?xml version="1.0"?><score/>']), 'good.xml')
    const r = await fetch(`${url}/api/music/upload-score`, { method: 'POST', body: fd, headers: bearer('alice') })
    assert.equal(r.status, 200)
    const body = await r.json() as { fileName: string }
    assert.equal(uploads.getUploadOwner(body.fileName), 'alice')
    const s = await fetch(`${url}/uploads/${body.fileName}`)
    assert.equal(s.headers.get('content-disposition'), 'attachment')
  })
})
