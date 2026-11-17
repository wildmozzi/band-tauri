// features/settings 공개 API — 오디오 설정·측정·진단·앱 업데이트. 다른 기능·app은 이 파일로만 import한다(docs/frontend-architecture.md).
import { lazy } from 'react'

/**
 * 오디오 설정 화면(모달·설정 페이지)은 지연 로딩한다(perf/d-web-load): 첫 화면(웹 관람·홈·방 목록)에는 필요 없고, 장치 목록·지연 측정·
 * 진단 내보내기·앱 업데이트까지 딸려 있다. 쓰는 쪽은 <Suspense>로 감싼다(App 라우트, Navigation 모달).
 * 컨텍스트(AudioSettingsProvider·useAudioSettings)와 합주실이 쓰는 EchoMeasure·DiagnosticsExport는 그대로 즉시 로드.
 */
export const AudioSettings = lazy(() => import('./AudioSettings').then(m => ({ default: m.AudioSettings })))
export const AudioSettingsPage = lazy(() => import('./AudioSettingsPage').then(m => ({ default: m.AudioSettingsPage })))
export { AudioSettingsProvider, useAudioSettings } from './AudioSettingsContext'
export { DiagnosticsExport } from './DiagnosticsExport'
export { EchoMeasure } from './EchoMeasure'
export { UpdateNotice } from './UpdateNotice'
