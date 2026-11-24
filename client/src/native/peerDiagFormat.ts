/**
 * 피어 통계 툴팁 진단(합주실 상세 roomDetailModel이 쓴다): 내 송신 코덱·자동 전환 이유(P1-4e), 손실 모양(P2-17h).
 * 웹 첫 화면 묶음에 실리므로 연주자 전용 진단 줄(engineDiagFormat — EngineDiagLines와 함께 performerUi 지연 묶음)과 파일을 나눈다(#250).
 */
import type { LossShapeStats, PeerStats } from './engineApi'

/** 자동 코덱 전환 이유(P1-4e) 문구. 모르는 값·없음이면 null */
export function txCodecReasonText(reason: PeerStats['txCodecReason']): string | null {
  if (reason === 'lossFeedback') return '상대 회선 손실 — 끊김 보정'
  if (reason === 'relayLoad') return '서버 경유 대역 절약'
  return null
}

/**
 * 이 피어에게 보내는 내 소리의 코덱·프레임: "내 송신: Opus · 5ms", 자동 전환이면 "(자동 전환)" — 이유를 알면 "(자동 전환: 이유)"로
 * 한 줄에 합친다(#250 L3). 아직 안 보냈으면 "내 송신: 아직 없음", 옛 엔진(필드 없음)이면 null
 */
export function formatTxCodec(ps: Pick<PeerStats, 'txCodec' | 'txCodecAuto' | 'txCodecReason'>, txFrameMs?: number | null): string | null {
  if (ps.txCodec === undefined) return null
  if (ps.txCodec === null) return '내 송신: 아직 없음'
  const codec = ps.txCodec === 'opus' ? 'Opus' : 'PCM16'
  const frame = txFrameMs ? ` · ${txFrameMs}ms` : ''
  const reason = txCodecReasonText(ps.txCodecReason)
  const auto = ps.txCodecAuto ? (reason ? ` (자동 전환: ${reason})` : ' (자동 전환)') : ''
  return `내 송신: ${codec}${frame}${auto}`
}

/** 손실 모양 연속 길이 버킷(패킷, P2-17h) */
export const LOSS_RUN_BUCKETS = ['1', '2', '3–4', '5–8', '9–16', '17+'] as const
/** 도착 여유 버킷(ms, P2-17h) */
export const ARRIVAL_MARGIN_BUCKETS = ['<0', '0–0.5', '0.5–1', '1–2', '2–2.5', '2.5–3', '3–5', '5–7.5', '7.5–10', '≥10'] as const
/** 도착 여유 버킷의 아래 경계(ms). 첫 버킷(<0)은 경계 없음 */
const MARGIN_LOWER = [-Infinity, 0, 0.5, 1, 2, 2.5, 3, 5, 7.5, 10]

const pct = (n: number, total: number) => (total > 0 ? `${((n * 100) / total).toFixed(1)}%` : '-')

/** 여유가 `ms` 이상인 수(경계가 버킷 경계와 맞을 때만, 아니면 null) */
function marginAtLeast(m: number[], ms: number): number | null {
  const i = MARGIN_LOWER.findIndex((e) => Math.abs(e - ms) < 1e-3)
  return i < 0 ? null : m.slice(i).reduce((a, b) => a + b, 0)
}

/**
 * 손실 모양 진단 줄(P2-17h, 피어 통계 툴팁): 손실·마감 놓침 연속 길이 표, 도착 여유(늦음 / 1프레임·2프레임 이상 비율), 은닉 방식.
 * 도착 여유는 직접 잰 "은닉 마감 − 도착"이고, ≥ 1F 비율이 "1프레임 늦게 보낸 복사본/FEC가 마감 안에 드는 비율"이다. 프레임이
 * 버킷 경계(2.5·5·10ms)가 아니면(128샘플 2.67ms 등) 그 비율은 셀 수 없다고 적는다. 옛 엔진(필드 없음)이면 빈 배열
 */
export function lossShapeLines(sh: LossShapeStats | null | undefined): string[] {
  if (!sh) return []
  const runs = (r: number[], longPkts?: number) =>
    LOSS_RUN_BUCKETS.map((b, i) => `${b}:${r[i] ?? 0}${i === LOSS_RUN_BUCKETS.length - 1 && longPkts ? `(${longPkts}패킷)` : ''}`).join(' ')
  const m = sh.arrivalMargin
  const total = m.reduce((a, b) => a + b, 0)
  const f = sh.frameMs
  const margin = [`늦음 ${pct(m[0] ?? 0, total)}`]
  for (const [k, label] of [[1, '1F'], [2, '2F']] as const) {
    const ms = Number((f * k).toFixed(2))
    const n = marginAtLeast(m, f * k)
    if (n !== null) margin.push(`≥${label}(${ms}ms) ${pct(n, total)}`)
    else if (k === 1) margin.push(`≥1F(${ms}ms) 버킷 경계 아님`)
  }
  return [
    `손실 연속(패킷): ${runs(sh.lossRuns, sh.lossLongPkts)}`,
    `마감 놓침 연속(패킷): ${runs(sh.lateRuns)}`,
    `도착 여유(마감−도착, ${total}개): ${margin.join(' · ')}`,
    `은닉 방식: FEC ${sh.concealFec} · PLC ${sh.concealPlc} · PCM ${sh.concealPcm} (언더런 ${sh.concealUnderrun})`,
  ]
}
