// features/settings 공개 API — 오디오 설정·측정·진단·앱 업데이트. 다른 기능·app은 이 파일로만 import한다(docs/frontend-architecture.md).
import { lazy } from 'react'
import { preloadableModule } from '../../shared/lib/preloadable'

/**
 * 오디오 설정 화면(모달·설정 페이지)은 지연 로딩한다(perf/d-web-load): 첫 화면(웹 관람·홈·방 목록)에는 필요 없고, 장치 목록·지연 측정·
 * 진단 내보내기·앱 업데이트까지 딸려 있다. 쓰는 쪽은 <Suspense>로 감싼다(App 라우트, Navigation 모달).
 * 컨텍스트(AudioSettingsProvider·useAudioSettings)와 합주실이 쓰는 DiagnosticsExport는 그대로 즉시 로드.
 */
export const AudioSettings = lazy(() => import('./AudioSettings').then(m => ({ default: m.AudioSettings })))
export const AudioSettingsPage = lazy(() => import('./AudioSettingsPage').then(m => ({ default: m.AudioSettingsPage })))
export { AudioSettingsProvider, useAudioSettings } from './AudioSettingsContext'
export { DiagnosticsExport } from './DiagnosticsExport'
// 피어 에코 측정 CSS는 첫 묶음 예전 자리에 둔다(JS만 지연 — 아래 EchoMeasure, 지연 CSS로 옮기면 cascade 순서가 바뀐다)
import './LatencyMeasure.css'

/**
 * 피어 에코 측정(연주자 채널 메뉴)은 연주 중(데스크톱 앱)에만 그린다 — 웹 첫 묶음에서 빼고, 데스크톱 앱은 첫 그리기 전에
 * preloadSettingsPerformerUi()로 받아 둔다(features/room의 preloadPerformerUi가 같이 부른다).
 */
const echoMeasure = preloadableModule(() => import('./EchoMeasure'))
export const EchoMeasure = echoMeasure.component('EchoMeasure')
export const preloadSettingsPerformerUi = (): Promise<unknown> => echoMeasure.preload()
export { UpdateNotice } from './UpdateNotice'
