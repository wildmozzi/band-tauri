// features/settings 공개 API — 오디오 설정·측정·진단·앱 업데이트. 다른 기능·app은 이 파일로만 import한다(docs/frontend-architecture.md).
import { createElement, lazy } from 'react'
import { isNativeApp } from '../../native/engineApi'
import { preloadableModule } from '../../shared/lib/preloadable'
import type { DiagnosticsExportProps } from './DiagnosticsExport'

/**
 * 오디오 설정 화면(모달·설정 페이지)은 지연 로딩한다(perf/d-web-load): 첫 화면(웹 관람·홈·방 목록)에는 필요 없고, 장치 목록·지연 측정·
 * 진단 내보내기·앱 업데이트까지 딸려 있다. 쓰는 쪽은 <Suspense>로 감싼다(App 라우트, Navigation 모달).
 * 컨텍스트(AudioSettingsProvider·useAudioSettings)는 그대로 즉시 로드.
 */
export const AudioSettings = lazy(() => import('./AudioSettings').then(m => ({ default: m.AudioSettings })))
export const AudioSettingsPage = lazy(() => import('./AudioSettingsPage').then(m => ({ default: m.AudioSettingsPage })))
export { AudioSettingsProvider, useAudioSettings } from './AudioSettingsContext'
// 진단 내보내기·피어 에코 측정 CSS는 첫 묶음 예전 자리에 둔다(JS만 지연 — 아래, 지연 CSS로 옮기면 cascade 순서가 바뀐다)
import './DiagnosticsExport.css'
import './LatencyMeasure.css'

/**
 * 피어 에코 측정(연주자 채널 메뉴)은 연주 중(데스크톱 앱)에만 그린다 — 웹 첫 묶음에서 빼고, 데스크톱 앱은 첫 그리기 전에
 * preloadSettingsPerformerUi()로 받아 둔다(main.tsx가 features/room의 preloadPerformerUi와 같이 부른다).
 */
const echoMeasure = preloadableModule(() => import('./EchoMeasure'))
export const EchoMeasure = echoMeasure.component('EchoMeasure')

/**
 * 진단 내보내기 버튼(합주실 믹서 헤더)도 데스크톱 앱에서만 보인다 — 같은 방식으로 웹 첫 묶음에서 뺀다. 웹에서는 지연 묶음을 받지
 * 않도록 여기서 먼저 걸러 null을 그린다(DiagnosticsExport 자체도 웹이면 null). 설정 화면(AudioSettings)은 직접 import한다.
 */
const diagnosticsExport = preloadableModule(() => import('./DiagnosticsExport'))
const LazyDiagnosticsExport = diagnosticsExport.component('DiagnosticsExport')
export function DiagnosticsExport(props: DiagnosticsExportProps) {
  return isNativeApp() ? createElement(LazyDiagnosticsExport, props) : null
}

export async function preloadSettingsPerformerUi(): Promise<void> {
  await Promise.all([echoMeasure.preload(), diagnosticsExport.preload()])
}
// 앱 업데이트 배너: 웹에서는 받지 않고 데스크톱에서만 지연 로드(UpdateNoticeGate)
export { UpdateNotice } from './UpdateNoticeGate'
