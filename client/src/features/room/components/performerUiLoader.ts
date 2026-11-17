/**
 * 연주자 전용 UI 지연 로드(웹 관람 첫 묶음 분리, #216 후속). 연주는 데스크톱 앱에서만 하므로 이 조각들은 웹에서 그려지지 않는다.
 * 데스크톱 앱은 첫 그리기 전에 preloadPerformerUi()를 기다려(main.tsx) 처음부터 바로 그린다 — 연주 시작 뒤 깜빡임 없음.
 */
import { preloadableModule } from '../../../shared/lib/preloadable'

const performerUi = preloadableModule(() => import('./performerUi'))

export const EngineDiagLines = performerUi.component('EngineDiagLines')
export const MetronomePanel = performerUi.component('MetronomePanel')
export const MyPerformerItem = performerUi.component('MyPerformerItem')
export const NativeRecordingControl = performerUi.component('NativeRecordingControl')

/** 합주실의 연주자 전용 UI를 받아 둔다(설정 기능의 피어 에코 측정은 features/settings의 preloadSettingsPerformerUi — main.tsx가 같이 부른다).
 * 여기서 settings를 import하지 않는다: 그러면 settings 공개 API 모듈이 앞당겨 평가돼 첫 묶음 CSS 순서가 바뀐다 */
export async function preloadPerformerUi(): Promise<void> {
  await performerUi.preload()
}
