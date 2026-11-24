// 업데이트 본체(native/updater)는 데스크톱에서만 지연 로드된다 — 로드 전에 RoomProvider가 알린 "방 안" 상태가 사라지지 않는지
import { beforeEach, describe, expect, it, vi } from 'vitest'

const h = vi.hoisted(() => ({
  update: {
    version: '0.2.0',
    body: '변경 사항',
    download: async () => {},
    install: async () => {},
  },
}))

vi.mock('@tauri-apps/plugin-updater', () => ({ check: async () => h.update }))
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: async () => {} }))
vi.mock('./engineApi', () => ({
  isNativeApp: () => true,
  engineErrorMessage: (e: unknown) => String(e),
  engineStats: async () => ({ running: false }),
}))

beforeEach(() => {
  vi.resetModules()
})

describe('updaterGate', () => {
  it('bind 전에 들어온 값을 bind 때 넘기고, 이후 변경도 넘긴다', async () => {
    const gate = await import('./updaterGate')
    gate.setUpdaterInRoom(true)
    const seen: boolean[] = []
    gate.bindUpdaterInRoom((v) => seen.push(v))
    gate.setUpdaterInRoom(false)
    expect(seen).toEqual([true, false])
  })

  it('본체 로드 전에 방에 들어갔으면 로드 뒤 확인한 업데이트는 보류(deferred)되고, 방을 나오면 다시 알린다', async () => {
    const gate = await import('./updaterGate')
    gate.setUpdaterInRoom(true) // 지연 청크를 받기 전 — RoomProvider가 먼저 부른다
    const { appUpdater } = await import('./updater')
    await appUpdater.checkForUpdate({ silent: true })
    expect(appUpdater.getStatus()).toMatchObject({ kind: 'deferred', version: '0.2.0' })
    gate.setUpdaterInRoom(false)
    expect(appUpdater.getStatus()).toMatchObject({ kind: 'available', version: '0.2.0' })
  })

  it('방 밖에서 로드하면 바로 available', async () => {
    const { appUpdater } = await import('./updater')
    await appUpdater.checkForUpdate({ silent: true })
    expect(appUpdater.getStatus()).toMatchObject({ kind: 'available' })
  })
})
