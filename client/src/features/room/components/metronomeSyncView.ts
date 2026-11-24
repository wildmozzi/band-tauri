import type { MetronomeDiag } from '../state/engineStatsPicks'

/**
 * 메트로놈 동기 진단 줄의 글(TEST-SESSION 3.5절 5번 — 예전에는 개발자 도구 Console로 읽던 `engine_stats().metronome` 값).
 * 줄에는 시험자가 판정에 쓰는 값만: 동기 여부, 시계 RTT·기울기(따르는 쪽), 늦은·뺀·옮긴 클릭.
 * `clockOffsetMs`(기준 피어 시계 − 내 시계)는 툴팁에만 둔다 — 엔진 시계가 엔진 시작부터 흐른 시간이라 따르는 쪽에서는
 * 두 PC의 엔진 시작 시각 차(수 초~수 분)가 나오고, 동기 오차가 아니다(#253 리뷰 M-1).
 * 메트로놈이 꺼졌으면 null.
 */
export interface MetronomeSyncView {
  text: string
  title: string
  /** 늦은 클릭이 있거나 시계를 맞출 수 없음(옛 엔진 방장·기준 없음). 뺀 박은 BPM 변경 때도 생겨 경고로 보지 않는다 */
  warn: boolean
}

const ms = (v: number | null, digits: number, signed = false) =>
  v == null ? '—' : `${signed && v > 0 ? '+' : ''}${v.toFixed(digits)}ms`
const ppm = (v: number | null) => (v == null ? '—' : `${v > 0 ? '+' : ''}${v.toFixed(1)}ppm`)
const count = (v: number | null) => (v == null ? '—' : String(v))

/** isHost: 패널이 아는 방장 여부(엔진의 refPeerId 유무로 추론하지 않는다, #253 리뷰 L-1) */
export function metronomeSyncView(d: MetronomeDiag, isHost: boolean): MetronomeSyncView | null {
  if (!d.running) return null
  const clicks = `늦은 클릭 ${count(d.lateClicks)} · 뺀 박 ${count(d.skippedClicks)} · 옮긴 클릭 ${count(d.shiftedClicks)}`
  let clock: string
  let clockFail = false
  if (isHost) {
    clock = '시계 기준(방장)'
  } else if (!d.refPeer) {
    clock = '기준 시계 없음(방장 계획 대기)'
    clockFail = true
  } else if (d.refClockUnsupported) {
    clock = '시계 맞출 수 없음(방장 앱이 옛 버전)'
    clockFail = true
  } else {
    clock = `${d.synced ? '동기됨' : '시계 맞추는 중'} · 시계 RTT ${ms(d.clockRttMs, 1)} · 기울기 ${ppm(d.driftPpm)}`
  }
  const follower = !isHost && d.refPeer
  const title = [
    `동기(synced): ${d.synced ? '맞춤' : '맞추는 중'}`,
    isHost ? '내가 기준 시계(방장)' : `방장 시계와 왕복(clockRttMs): ${ms(d.clockRttMs, 1)}`,
    follower && `시계 기울기(driftPpm): ${ppm(d.driftPpm)}`,
    follower &&
      `엔진 시작 시각 차(clockOffsetMs = 방장 시계 − 내 시계): ${ms(d.clockOffsetMs, 2, true)} — 두 PC가 엔진을 켠 시각 차이라 수 초~수 분이 정상(동기 오차 아님)`,
    `늦은 클릭(lateClicks): ${count(d.lateClicks)}`,
    `뺀 박(skippedClicks): ${count(d.skippedClicks)} — 자리가 20ms 넘게 지났거나 이중 클릭 방지(BPM 변경 때도 생김)`,
    `옮긴 클릭(shiftedClicks): ${count(d.shiftedClicks)} — 1ms 넘게 어긋나게 울린 클릭`,
  ]
    .filter(Boolean)
    .join('\n')
  return { text: `${clock} · ${clicks}`, title, warn: (d.lateClicks ?? 0) > 0 || clockFail }
}
