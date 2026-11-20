import type { MetronomeDiag } from '../state/engineStatsPicks'

/**
 * 메트로놈 동기 진단 줄의 글(TEST-SESSION 3.5절 5번 — 예전에는 개발자 도구 Console로 읽던 `engine_stats().metronome` 값).
 * 줄: 시계 왕복·오프셋(따르는 쪽) 또는 기준 시계(방장), 늦은 클릭·뺀 박. 툴팁: 같은 값 + 엔진 필드 이름, 기울기·옮긴 클릭.
 * 메트로놈이 꺼졌으면 null.
 */
export interface MetronomeSyncView {
  text: string
  title: string
  /** 늦은 클릭이 있거나 옛 엔진 방장이라 동기화할 수 없음(뺀 박은 BPM 변경 때도 생겨 경고로 보지 않는다) */
  warn: boolean
}

const ms = (v: number | null, digits: number, signed = false) =>
  v == null ? '—' : `${signed && v > 0 ? '+' : ''}${v.toFixed(digits)}ms`
const count = (v: number | null) => (v == null ? '—' : String(v))

export function metronomeSyncView(d: MetronomeDiag): MetronomeSyncView | null {
  if (!d.running) return null
  const late = count(d.lateClicks)
  const skipped = count(d.skippedClicks)
  const clock = !d.refPeer
    ? '시계 기준(방장)'
    : d.refClockUnsupported
      ? '시계 맞출 수 없음(방장 앱이 옛 버전)'
      : `시계 RTT ${ms(d.clockRttMs, 1)} · 오프셋 ${ms(d.clockOffsetMs, 2, true)}`
  const text = `${clock} · 늦은 클릭 ${late} · 뺀 박 ${skipped}`
  const title = [
    `동기(synced): ${d.synced ? '맞춤' : '맞추는 중'}`,
    d.refPeer ? `방장 시계와 왕복(clockRttMs): ${ms(d.clockRttMs, 1)}` : '내가 기준 시계(방장)',
    d.refPeer && `방장 시계 − 내 시계(clockOffsetMs): ${ms(d.clockOffsetMs, 2, true)}`,
    d.refPeer && `시계 기울기(driftPpm): ${d.driftPpm == null ? '—' : `${d.driftPpm.toFixed(1)}ppm`}`,
    `늦은 클릭(lateClicks): ${late}`,
    `뺀 박(skippedClicks): ${skipped} — 자리가 20ms 넘게 지났거나 이중 클릭 방지`,
    `옮긴 클릭(shiftedClicks): ${count(d.shiftedClicks)} — 1ms 넘게 어긋나게 울린 클릭`,
  ]
    .filter(Boolean)
    .join('\n')
  const warn = (d.lateClicks ?? 0) > 0 || (d.refPeer && d.refClockUnsupported)
  return { text, title, warn }
}
