import type { Request, Response, NextFunction } from 'express'
import { supabaseAdmin } from './supabase'

/** access token → userId (검증 실패 시 null) */
export type TokenVerifier = (token: string) => Promise<string | null>

const supabaseVerifier: TokenVerifier = async (token) => {
  if (!supabaseAdmin) return null
  try {
    const { data, error } = await supabaseAdmin.auth.getUser(token)
    if (error || !data.user) return null
    return data.user.id
  } catch {
    return null
  }
}

let verifier: TokenVerifier = supabaseVerifier

/** 테스트용: 토큰 검증기 교체 (인자 없으면 Supabase 검증기로 복구) */
export function setTokenVerifier(v?: TokenVerifier) {
  verifier = v ?? supabaseVerifier
}

export function verifyAccessToken(token: string): Promise<string | null> {
  return verifier(token)
}

export function getBearerToken(authHeader: string | string[] | undefined): string | null {
  const header = Array.isArray(authHeader) ? authHeader[0] : authHeader
  if (!header || !header.startsWith('Bearer ')) return null
  const token = header.slice(7).trim()
  return token && token.length <= 8192 ? token : null
}

/** Authorization: Bearer 헤더를 검증해 userId 반환 (없거나 실패 시 null) */
export async function getUserIdFromRequest(req: Request): Promise<string | null> {
  const token = getBearerToken(req.headers.authorization)
  if (!token) return null
  return verifyAccessToken(token)
}

/** 인증 필수 미들웨어. 성공 시 res.locals.userId 에 서버가 확정한 userId를 넣는다 */
export async function requireAuth(req: Request, res: Response, next: NextFunction) {
  const userId = await getUserIdFromRequest(req)
  if (!userId) {
    res.status(401).json({ success: false, error: '로그인이 필요합니다.' })
    return
  }
  res.locals.userId = userId
  next()
}
