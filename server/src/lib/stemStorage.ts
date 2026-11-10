/**
 * 음원분리 스템 Storage 경로 규칙 (업로드·삭제·만료 정리·마이그레이션 공통)
 *
 * 경로: `${storage_folder}/${stemName}.${ext}` — ext는 'ogg'(P3-6 이후) 또는 'wav'(이전 기록/인코딩 폴백).
 * DB에는 포맷 컬럼이 없다. 포맷은 URL/경로 확장자로 판별한다.
 */

export const STEM_BUCKET = 'audio-stems'
export const STEM_NAMES = ['vocals', 'drums', 'bass', 'piano', 'guitar', 'other'] as const
export type StemName = typeof STEM_NAMES[number]
export const STEM_EXTENSIONS = ['ogg', 'wav'] as const

/** URL 또는 경로에서 스템 포맷(확장자) 판별. 쿼리/해시는 무시. */
export function stemFormatOf(urlOrPath: string): 'ogg' | 'wav' | null {
  const clean = urlOrPath.split(/[?#]/)[0].toLowerCase()
  if (clean.endsWith('.ogg') || clean.endsWith('.opus')) return 'ogg'
  if (clean.endsWith('.wav')) return 'wav'
  return null
}

/** 한 분리 결과 폴더에서 지워야 할 모든 스템 경로 (포맷 무관, 없는 경로는 Storage가 무시). */
export function stemStoragePaths(folder: string): string[] {
  const out: string[] = []
  for (const name of STEM_NAMES) {
    for (const ext of STEM_EXTENSIONS) out.push(`${folder}/${name}.${ext}`)
  }
  return out
}

/**
 * 공개 URL → 버킷 내 경로. 예:
 * https://x.supabase.co/storage/v1/object/public/audio-stems/u/sep/vocals.wav → u/sep/vocals.wav
 */
export function storagePathFromPublicUrl(url: string): string | null {
  const marker = `/object/public/${STEM_BUCKET}/`
  const clean = url.split(/[?#]/)[0]
  const i = clean.indexOf(marker)
  if (i < 0) return null
  return decodeURIComponent(clean.slice(i + marker.length))
}

/**
 * DB `*_url` 컬럼에 저장하는 드라이버 중립 참조 (P3-10). 공개 URL이 없는 저장소(S3 비공개 버킷)에서 쓴다.
 * 클라이언트에는 서버가 서명 GET URL로 바꿔 내려준다.
 */
export const STEM_REF_PREFIX = `storage://${STEM_BUCKET}/`

/**
 * DB `*_url` 값 → 버킷 내 key. 받는 형식:
 *   - `storage://audio-stems/<key>` (P3-10 이후 S3 비공개)
 *   - 경로에 `/audio-stems/`가 있는 http(s) URL (Supabase 공개 URL, S3_PUBLIC_BASE_URL 공개 URL)
 * key는 반드시 그 기록의 폴더(`${folder}/스템.확장자`) 안이어야 한다. 아니면 null (다른 사용자 파일 서명 방지).
 */
export function stemKeyFromRef(ref: string, folder: string): string | null {
  let raw: string | null = null
  if (ref.startsWith(STEM_REF_PREFIX)) {
    raw = ref.slice(STEM_REF_PREFIX.length).split(/[?#]/)[0]
  } else if (/^https?:\/\//i.test(ref)) {
    let pathname: string
    try {
      pathname = new URL(ref).pathname
    } catch {
      return null
    }
    const marker = `/${STEM_BUCKET}/`
    const i = pathname.indexOf(marker)
    if (i < 0) return null
    raw = pathname.slice(i + marker.length)
  }
  if (raw === null) return null
  let key: string
  try {
    key = raw.split('/').map(decodeURIComponent).join('/')
  } catch {
    return null
  }
  const rest = key.startsWith(`${folder}/`) ? key.slice(folder.length + 1) : null
  if (rest === null || rest.includes('/')) return null
  const m = /^([a-z]+)\.([a-z0-9]+)$/.exec(rest)
  if (!m || !(STEM_NAMES as readonly string[]).includes(m[1]) || !(STEM_EXTENSIONS as readonly string[]).includes(m[2])) return null
  return key
}
