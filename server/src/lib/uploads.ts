import * as fs from 'fs'
import * as path from 'path'

/**
 * 업로드 파일 디렉터리와 안전한 경로 처리 (C1: 경로 순회 방지)
 * UPLOAD_DIR 환경변수로 바꿀 수 있다(테스트용). 기본값: <server>/uploads
 */
export const UPLOAD_DIR = path.resolve(process.env.UPLOAD_DIR || path.join(__dirname, '..', '..', 'uploads'))

/** 허용 파일명: 영문/숫자로 시작, 영문/숫자/._- 만. 경로 구분자·dotfile 불가 */
const SAFE_FILENAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,199}$/

export function ensureUploadDir() {
  if (!fs.existsSync(UPLOAD_DIR)) fs.mkdirSync(UPLOAD_DIR, { recursive: true })
}

/**
 * 클라이언트가 준 파일명을 검증하고 업로드 디렉터리 안의 절대경로로 변환한다.
 * 화이트리스트 → '..' 금지 → basename 일치 → resolve 후 업로드 디렉터리 접두 검사.
 * 하나라도 실패하면 null.
 */
export function resolveUploadPath(name: unknown): string | null {
  if (typeof name !== 'string') return null
  if (!SAFE_FILENAME.test(name)) return null
  if (name.includes('..')) return null
  if (path.basename(name) !== name) return null
  const full = path.resolve(UPLOAD_DIR, name)
  if (!full.startsWith(UPLOAD_DIR + path.sep)) return null
  return full
}

// ---------- 소유자 기록 (파일명 → userId) ----------
// 업로드 디렉터리의 dotfile에 저장 (SAFE_FILENAME이 dotfile을 막으므로 API/정적 서빙으로 노출되지 않음)

const OWNERS_FILE = path.join(UPLOAD_DIR, '.owners.json')
let owners: Record<string, string> | null = null

function loadOwners(): Record<string, string> {
  if (owners) return owners
  try {
    owners = JSON.parse(fs.readFileSync(OWNERS_FILE, 'utf8')) as Record<string, string>
  } catch {
    owners = {}
  }
  return owners
}

function saveOwners() {
  try {
    ensureUploadDir()
    fs.writeFileSync(OWNERS_FILE, JSON.stringify(owners ?? {}))
  } catch (err) {
    console.error('[Uploads] Failed to persist owner registry:', err)
  }
}

export function recordUploadOwner(filename: string, userId: string) {
  loadOwners()[filename] = userId
  saveOwners()
}

export function getUploadOwner(filename: string): string | null {
  return loadOwners()[filename] ?? null
}

export function removeUploadOwner(filename: string) {
  const o = loadOwners()
  if (filename in o) {
    delete o[filename]
    saveOwners()
  }
}

// ---------- 매직 바이트 검사 (M6) ----------

export function matchesFileSignature(filePath: string, ext: string): boolean {
  let head: Buffer
  try {
    const fd = fs.openSync(filePath, 'r')
    try {
      head = Buffer.alloc(512)
      const n = fs.readSync(fd, head, 0, 512, 0)
      head = head.subarray(0, n)
    } finally {
      fs.closeSync(fd)
    }
  } catch {
    return false
  }
  const e = ext.toLowerCase()
  if (e === '.pdf') return head.subarray(0, 5).toString('latin1') === '%PDF-'
  if (e === '.mid' || e === '.midi') return head.subarray(0, 4).toString('latin1') === 'MThd'
  if (e === '.mp3') {
    if (head.subarray(0, 3).toString('latin1') === 'ID3') return true
    return head.length >= 2 && head[0] === 0xff && (head[1] & 0xe0) === 0xe0
  }
  if (e === '.xml' || e === '.musicxml') {
    let text = head.toString('utf8')
    if (text.charCodeAt(0) === 0xfeff) text = text.slice(1)
    return text.trimStart().startsWith('<')
  }
  return false
}
