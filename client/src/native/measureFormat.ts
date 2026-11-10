/**
 * 지연 측정 결과(P1-1) 표·텍스트 변환. UI(오디오 설정 "지연 측정", 방 화면 "에코 측정")와
 * 결과 복사/내보내기, docs/MEASURE.md의 백로그 기록 형식이 같은 함수를 쓴다.
 */
import type { EchoReport, LoopbackReport, MeasureVerdict } from './engineApi'

export const VERDICT_LABEL: Record<MeasureVerdict, string> = {
  ok: '정상',
  noSignal: '신호 미검출',
  clipped: '클리핑',
  unstable: '불안정',
}

/** ms 값 표시. null/NaN이면 '—' */
export function fmtMs(v: number | null | undefined, digits = 2): string {
  return typeof v === 'number' && Number.isFinite(v) ? `${v.toFixed(digits)}ms` : '—'
}

function signed(v: number | null | undefined, digits = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '—'
  return `${v >= 0 ? '+' : ''}${v.toFixed(digits)}ms`
}

export interface MeasureRow {
  label: string
  value: string
  hint?: string
}

/** 드라이버 보고값의 의미는 백엔드마다 다르다(ASIO는 변환기·안전 여유 포함, WASAPI GetStreamLatency는 대개 장치 주기·버퍼만). */
function driverHint(backend: string): string | undefined {
  if (backend.includes('Asio')) return 'ASIO ASIOGetLatencies: 드라이버가 변환기·안전 여유까지 보고해야 0에 가깝다'
  if (backend.includes('wasapi')) return 'WASAPI GetStreamLatency는 변환기 지연을 포함하지 않아 실측보다 작은 게 보통이다'
  return undefined
}

/** 루프백 결과 표: 왕복·추정 편도·드라이버 보고·estLatency·오차 */
export function loopbackRows(r: LoopbackReport): MeasureRow[] {
  const oneWay = r.roundTripMs == null ? null : r.roundTripMs / 2
  return [
    { label: '판정', value: `${VERDICT_LABEL[r.verdict]} (검출 ${r.detectedRuns}/${r.runs.length}회)` },
    { label: '실측 왕복(중앙값)', value: fmtMs(r.roundTripMs), hint: `최소 ${fmtMs(r.minMs)} · 최대 ${fmtMs(r.maxMs)} · 표준편차 ${fmtMs(r.stdDevMs, 3)}` },
    { label: '추정 편도(왕복 ÷ 2)', value: fmtMs(oneWay) },
    { label: '장치 버퍼 왕복', value: fmtMs(r.bufferRoundTripMs), hint: `입력 ${r.inputBufferFrames} + 출력 ${r.outputBufferFrames}프레임 @ ${r.sampleRate}Hz` },
    {
      label: '드라이버 보고 왕복',
      value: fmtMs(r.driverRoundTripMs),
      hint: r.driverRoundTripMs == null ? '이 백엔드는 보고하지 않음' : `입력 ${fmtMs(r.driverInputLatencyMs)} + 출력 ${fmtMs(r.driverOutputLatencyMs)}`,
    },
    {
      label: '드라이버 보고 오차(실측 − 보고)',
      value: signed(r.driverErrorMs),
      hint: driverHint(r.backend),
    },
    { label: 'estLatencyMs(엔진 추정)', value: fmtMs(r.estLatencyMs) },
    { label: '보정값(실측 − 장치 버퍼)', value: signed(r.offsetMs), hint: 'estLatencyMs에 없는 변환기·드라이버 안전 여유' },
    { label: '보정한 로컬 지연', value: fmtMs(r.correctedEstLatencyMs), hint: 'estLatencyMs + 보정값' },
  ]
}

/** 에코 결과 표: 네트워크 포함 왕복, 편도 추정, 분해(내 로컬·망 RTT·지터 버퍼) */
export function echoRows(r: EchoReport): MeasureRow[] {
  return [
    { label: '판정', value: `${VERDICT_LABEL[r.verdict]} (클릭 ${r.detectedRuns}/${r.runs.length}개)` },
    { label: '네트워크 포함 왕복(중앙값)', value: fmtMs(r.roundTripMs, 1), hint: `최소 ${fmtMs(r.minMs, 1)} · 최대 ${fmtMs(r.maxMs, 1)} · 표준편차 ${fmtMs(r.stdDevMs)}` },
    { label: '상대 처리 지연(보고)', value: fmtMs(r.peerEchoDelayMs, 3) },
    { label: '편도 추정((왕복 − 상대 처리) ÷ 2)', value: fmtMs(r.oneWayMs, 1) },
    { label: '입→귀 추정', value: fmtMs(r.mouthToEarMs, 1), hint: '입력 버퍼 + 프레임 + 망 편도 + 지터 버퍼 + 출력 버퍼' },
    { label: '망 RTT(핑)', value: fmtMs(r.networkRttMs, 1), hint: r.path ? `경로 ${r.path}` : undefined },
    { label: '내 지터 버퍼', value: fmtMs(r.jitterBufferMs, 1) },
    { label: '프레임', value: fmtMs(r.frameMs, 1) },
    { label: '내 로컬(estLatencyMs)', value: fmtMs(r.localLatencyMs), hint: `입력 버퍼 ${fmtMs(r.inputBufferMs)} · 출력 버퍼 ${fmtMs(r.outputBufferMs)}` },
    { label: '잔차', value: signed(r.residualMs, 1), hint: '왕복 − (프레임 + 망 RTT + 상대 처리 + 지터 버퍼). 0에 가까우면 통계가 맞음' },
  ]
}

function rowsText(rows: MeasureRow[]): string {
  return rows.map((row) => `- ${row.label}: ${row.value}${row.hint ? ` (${row.hint})` : ''}`).join('\n')
}

export interface MeasureContext {
  /** 측정 시각(ISO). 없으면 지금 */
  at?: string
  /** 장치/드라이버 설명(예: "Scarlett Solo 4th Gen ASIO, 버퍼 64") */
  device?: string
}

/** 결과 복사/내보내기용 텍스트(루프백) */
export function loopbackText(r: LoopbackReport, ctx: MeasureContext = {}): string {
  const head = [
    '# 하드웨어 루프백 지연 측정',
    `측정: ${ctx.at ?? new Date().toISOString()}`,
    ctx.device ? `장치: ${ctx.device}` : null,
    `백엔드: ${r.backend || '—'}`,
    `메시지: ${r.message}`,
  ].filter(Boolean)
  const runs = r.runs.map((x, i) => `  ${i + 1}. ${x.roundTripMs == null ? '미검출' : fmtMs(x.roundTripMs, 3)} (피크비 ${x.peakRatio.toFixed(1)}, 상관 ${x.correlation.toFixed(2)}${x.clipped ? ', 클리핑' : ''}${x.inverted ? ', 극성 반전' : ''})`)
  return `${head.join('\n')}\n\n${rowsText(loopbackRows(r))}\n\n반복:\n${runs.join('\n')}\n\n백로그 기록: ${loopbackBacklogLine(r, ctx)}\n`
}

/** 결과 복사/내보내기용 텍스트(에코) */
export function echoText(r: EchoReport, ctx: MeasureContext & { peerName?: string } = {}): string {
  const head = [
    '# 에코 피어 지연 측정(네트워크 포함)',
    `측정: ${ctx.at ?? new Date().toISOString()}`,
    `상대: ${ctx.peerName ?? r.peerId}`,
    ctx.device ? `장치: ${ctx.device}` : null,
    `메시지: ${r.message}`,
  ].filter(Boolean)
  const runs = r.runs.map((v, i) => `  ${i + 1}. ${v == null ? '미검출' : fmtMs(v, 2)}`)
  return `${head.join('\n')}\n\n${rowsText(echoRows(r))}\n\n클릭:\n${runs.join('\n')}\n\n백로그 기록: ${echoBacklogLine(r, ctx)}\n`
}

function day(ctx: MeasureContext): string {
  return (ctx.at ?? new Date().toISOString()).slice(0, 10)
}

/**
 * BACKLOG.md 기록 한 줄(docs/MEASURE.md 6절 형식):
 * `[실측 YYYY-MM-DD] 루프백 <장치> <레이트>/<버퍼>: 왕복 Xms(σ), 드라이버 보고 Yms, est Zms, 보정 +Wms — 판정`
 */
export function loopbackBacklogLine(r: LoopbackReport, ctx: MeasureContext = {}): string {
  const dev = ctx.device ?? r.backend
  return `[실측 ${day(ctx)}] 루프백 ${dev} ${r.sampleRate}Hz/${r.inputBufferFrames}: 왕복 ${fmtMs(r.roundTripMs)}(σ ${fmtMs(r.stdDevMs, 3)}), 드라이버 보고 ${fmtMs(r.driverRoundTripMs)}, est ${fmtMs(r.estLatencyMs)}, 보정 ${signed(r.offsetMs)} — ${VERDICT_LABEL[r.verdict]}`
}

/** `[실측 YYYY-MM-DD] 에코 <상대>(<경로>): 왕복 Xms, 편도 Yms, 입→귀 Zms, 핑 Ams, 지터 버퍼 Bms — 판정` */
export function echoBacklogLine(r: EchoReport, ctx: MeasureContext & { peerName?: string } = {}): string {
  return `[실측 ${day(ctx)}] 에코 ${ctx.peerName ?? r.peerId}(${r.path ?? '—'}): 왕복 ${fmtMs(r.roundTripMs, 1)}, 편도 ${fmtMs(r.oneWayMs, 1)}, 입→귀 ${fmtMs(r.mouthToEarMs, 1)}, 핑 ${fmtMs(r.networkRttMs, 1)}, 지터 버퍼 ${fmtMs(r.jitterBufferMs, 1)} — ${VERDICT_LABEL[r.verdict]}`
}

/** 텍스트 파일 내보내기(브라우저 다운로드) */
export function downloadText(filename: string, text: string): void {
  const blob = new Blob([text], { type: 'text/plain;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

/** 클립보드 복사. 실패하면 false */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    return false
  }
}
