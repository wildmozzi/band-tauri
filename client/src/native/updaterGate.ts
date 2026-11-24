/**
 * 앱 업데이트의 첫 화면용 얇은 입구 — 업데이트 본체(native/updater.ts·@tauri-apps/plugin-updater)는 데스크톱에서만
 * 지연 청크로 받는다(웹 관람자는 받지 않음). 여기에는 정적으로 실어도 되는 가벼운 것만 둔다.
 *
 * - isUpdaterAvailable: 데스크톱 앱에서만 업데이트가 의미 있다(웹 관람자는 항상 최신).
 * - 방 참여 여부: RoomProvider는 본체가 로드되기 전에도 setUpdaterInRoom을 부른다. 값을 여기서 들고 있다가
 *   본체가 로드되며 bindUpdaterInRoom으로 이어 받는다 — 로드 전에 들어온 "방 안" 상태가 사라지지 않게(연주 중 설치 보류).
 */
import { isNativeApp } from './engineApi'

export function isUpdaterAvailable(): boolean {
  return isNativeApp()
}

let inRoom = false
let sink: ((inRoom: boolean) => void) | null = null

/** 방 참여 여부(엔진 시작 전 대기 포함). 본체가 로드돼 있으면 바로 넘기고, 아니면 값만 기억한다. */
export function setUpdaterInRoom(next: boolean): void {
  inRoom = next
  sink?.(next)
}

/** 업데이트 본체가 로드될 때 한 번 부른다 — 지금까지의 값을 즉시 넘기고 이후 변경을 받는다. */
export function bindUpdaterInRoom(fn: (inRoom: boolean) => void): void {
  sink = fn
  fn(inRoom)
}
