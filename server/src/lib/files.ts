import * as fs from 'fs'

// Multer이 latin1로 파일명을 읽는 경우 UTF-8로 변환 (이미 한글이면 그대로)
export function tryDecodeFilename(name: string): string {
  if (/[가-힣ᄀ-ᇿ㄰-㆏]/.test(name)) return name
  try {
    const decoded = Buffer.from(name, 'latin1').toString('utf8')
    if (/[가-힣ᄀ-ᇿ㄰-㆏]/.test(decoded)) return decoded
  } catch {}
  return name
}

/** 있으면 지운다 (실패 무시) */
export function safeUnlink(p: string | null | undefined) {
  if (!p) return
  try {
    if (fs.existsSync(p)) fs.unlinkSync(p)
  } catch {
    // 무시
  }
}
