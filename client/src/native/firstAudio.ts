/**
 * "연주 시작 → 첫 피어 소리"(P1-20, GOAL 3-6 "입장 후 3초 이내 소리") 계산. 순수 함수만 — 시각은 호출자가 넘긴다.
 *
 * - 엔진은 피어마다 `PeerStats.firstAudioMs`(engine_add_peer → 그 피어의 실제 소리가 처음 재생 링에 들어간 순간)를 준다.
 * - 클라이언트는 연주 시작 시각(`startAt`)과 피어마다 engine_add_peer를 부른 시각(`addedAt`)을 안다.
 *   그 피어가 들린 시각 = addedAt + firstAudioMs. 기준은 시작 때 이미 있던 연주자면 `startAt`(서버 왕복·descriptor
 *   교환 포함), 시작 뒤에 들어온 연주자면 그 피어를 처음 추가한 시각(그 사람의 입장)이다.
 * - 한 번 들린 피어의 값은 고정한다(재추가·경로 전환으로 다시 재지 않는다). 엔진을 다시 시작하면 새로 잰다.
 */

import type { PeerStats } from './engineApi'

/** GOAL 3-6: 입장 후 3초 이내 소리 */
export const FIRST_AUDIO_GOAL_MS = 3000

export interface FirstAudioTrack {
  /** 연주 시작(엔진 시작 요청) 시각, ms(단조 시계) */
  startAt: number
  /** 시작할 때 이미 알고 있던 다른 연주자 */
  startPeers: ReadonlySet<string>
  /** 피어별 기준 시각(처음 추가할 때 정한다) */
  base: ReadonlyMap<string, number>
  /** 피어별 마지막 engine_add_peer 호출 시각(엔진 firstAudioMs의 기준) */
  addedAt: ReadonlyMap<string, number>
  /** 피어별 확정된 "기준 → 첫 소리"(ms) */
  heard: ReadonlyMap<string, number>
}

export function newFirstAudioTrack(startAt: number, startPeers: Iterable<string>): FirstAudioTrack {
  return { startAt, startPeers: new Set(startPeers), base: new Map(), addedAt: new Map(), heard: new Map() }
}

/** engine_add_peer를 `now`에 부른다. 기준 시각은 처음 추가할 때만 정한다 */
export function noteAddPeer(t: FirstAudioTrack, peerId: string, now: number): FirstAudioTrack {
  const addedAt = new Map(t.addedAt).set(peerId, now)
  if (t.base.has(peerId)) return { ...t, addedAt }
  const base = new Map(t.base).set(peerId, t.startPeers.has(peerId) ? t.startAt : now)
  return { ...t, base, addedAt }
}

/** 엔진 통계에서 새로 들린 피어를 확정한다. 바뀐 게 없으면 같은 객체를 돌려준다 */
export function noteStats(t: FirstAudioTrack, peers: readonly PeerStats[]): FirstAudioTrack {
  let heard: Map<string, number> | null = null
  for (const p of peers) {
    if (t.heard.has(p.peerId) || p.firstAudioMs == null || !Number.isFinite(p.firstAudioMs)) continue
    const added = t.addedAt.get(p.peerId)
    const base = t.base.get(p.peerId)
    if (added === undefined || base === undefined) continue
    heard ??= new Map(t.heard)
    heard.set(p.peerId, Math.max(0, added + p.firstAudioMs - base))
  }
  return heard ? { ...t, heard } : t
}

export interface FirstAudioSummary {
  /** 가장 빨리 들린 피어(ms). 아직 아무도 안 들렸으면 null */
  firstMs: number | null
  /** 가장 늦게 들린 피어(ms, 지금까지 들린 피어 중) */
  lastMs: number | null
  /** 지금 엔진에 있는 피어 중 들린 수 / 전체 */
  heard: number
  total: number
}

/** 지금 엔진에 있는 피어 기준 요약. 피어가 없으면 null */
export function summarizeFirstAudio(t: FirstAudioTrack | null, peers: readonly PeerStats[]): FirstAudioSummary | null {
  if (!t || peers.length === 0) return null
  const values = peers.map((p) => t.heard.get(p.peerId)).filter((v): v is number => v !== undefined)
  return {
    firstMs: values.length ? Math.min(...values) : null,
    lastMs: values.length ? Math.max(...values) : null,
    heard: values.length,
    total: peers.length,
  }
}

const sec = (ms: number) => `${(ms / 1000).toFixed(2)}초`

/** 진단 줄 문구. 예: "첫 소리 0.82초", "첫 소리 0.82~1.40초 (2/3명)", "첫 소리 대기 (0/2명)" */
export function formatFirstAudio(s: FirstAudioSummary | null): string | null {
  if (!s) return null
  const count = s.heard < s.total || s.total > 1 ? ` (${s.heard}/${s.total}명)` : ''
  if (s.firstMs === null || s.lastMs === null) return `첫 소리 대기${count}`
  const range = s.lastMs - s.firstMs >= 5 ? `${sec(s.firstMs)}~${sec(s.lastMs)}` : sec(s.firstMs)
  return `첫 소리 ${range}${count}`
}

/** 목표(3초)를 넘긴 피어가 있다 */
export function firstAudioOverGoal(s: FirstAudioSummary | null): boolean {
  return !!s && s.lastMs !== null && s.lastMs > FIRST_AUDIO_GOAL_MS
}

export const FIRST_AUDIO_TITLE =
  '연주 시작(뒤에 들어온 연주자는 그 사람의 입장) → 그 피어 소리가 처음 재생된 시간. 가장 빠른~가장 늦은 피어. 목표 3초 이내(GOAL 3-6)'
