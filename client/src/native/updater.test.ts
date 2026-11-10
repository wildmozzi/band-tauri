import { describe, expect, it, vi } from 'vitest'
import { createUpdater, type DownloadEvent, type UpdateHandle, type UpdaterDeps } from './updater'

function fakeUpdate(version = '0.2.0', events: DownloadEvent[] = []): UpdateHandle & { installed: number } {
  const u = {
    version,
    body: '변경 사항',
    installed: 0,
    async downloadAndInstall(onEvent?: (e: DownloadEvent) => void) {
      events.forEach((e) => onEvent?.(e))
      u.installed++
    },
  }
  return u
}

function deps(over: Partial<UpdaterDeps> = {}): UpdaterDeps {
  return {
    check: vi.fn(async () => null),
    relaunch: vi.fn(async () => {}),
    isEngineRunning: vi.fn(async () => false),
    ...over,
  }
}

describe('createUpdater', () => {
  it('업데이트 없음 → 수동 확인은 upToDate, 자동(silent) 확인은 idle 유지', async () => {
    const u = createUpdater(deps())
    await u.checkForUpdate({ silent: true })
    expect(u.getStatus()).toEqual({ kind: 'idle' })
    await u.checkForUpdate()
    expect(u.getStatus()).toEqual({ kind: 'upToDate' })
  })

  it('확인 실패 → 수동은 error, silent는 조용히 넘어간다(오프라인 시작)', async () => {
    const u = createUpdater(deps({ check: vi.fn(async () => Promise.reject(new Error('offline'))) }))
    await u.checkForUpdate({ silent: true })
    expect(u.getStatus().kind).toBe('idle')
    await u.checkForUpdate()
    expect(u.getStatus()).toEqual({ kind: 'error', message: 'offline' })
  })

  it('엔진이 멈춰 있으면 available → 설치 시 진행률을 거쳐 설치 후 재시작', async () => {
    const update = fakeUpdate('0.2.0', [
      { event: 'Started', data: { contentLength: 200 } },
      { event: 'Progress', data: { chunkLength: 120 } },
      { event: 'Progress', data: { chunkLength: 80 } },
      { event: 'Finished' },
    ])
    const d = deps({ check: vi.fn(async () => update) })
    const u = createUpdater(d)
    const seen: string[] = []
    u.subscribe(() => {
      const s = u.getStatus()
      seen.push(s.kind === 'downloading' ? `dl:${s.downloaded}/${s.total}` : s.kind)
    })

    await u.checkForUpdate({ silent: true })
    expect(u.getStatus()).toEqual({ kind: 'available', version: '0.2.0', notes: '변경 사항' })

    await u.installUpdate()
    expect(update.installed).toBe(1)
    expect(d.relaunch).toHaveBeenCalledOnce()
    expect(seen).toEqual(['available', 'dl:0/null', 'dl:0/200', 'dl:120/200', 'dl:200/200', 'installing', 'installing'])
  })

  it('연주 중이면 설치를 미루고 알림만 한다 (deferred), 연주가 끝나면 설치', async () => {
    let running = true
    const update = fakeUpdate()
    const d = deps({ check: vi.fn(async () => update), isEngineRunning: vi.fn(async () => running) })
    const u = createUpdater(d)

    await u.checkForUpdate({ silent: true })
    expect(u.getStatus().kind).toBe('deferred')

    await u.installUpdate()
    expect(u.getStatus().kind).toBe('deferred')
    expect(update.installed).toBe(0)
    expect(d.relaunch).not.toHaveBeenCalled()

    running = false
    await u.installUpdate()
    expect(update.installed).toBe(1)
    expect(d.relaunch).toHaveBeenCalledOnce()
  })

  it('설치 실패는 error로 알린다', async () => {
    const update = fakeUpdate()
    update.downloadAndInstall = async () => Promise.reject('서명 검증 실패')
    const d = deps({ check: vi.fn(async () => update) })
    const u = createUpdater(d)
    await u.checkForUpdate()
    await u.installUpdate()
    expect(u.getStatus()).toEqual({ kind: 'error', message: '업데이트 설치 실패: 서명 검증 실패' })
    expect(d.relaunch).not.toHaveBeenCalled()
  })

  it('확인 전 설치 요청은 무시', async () => {
    const d = deps()
    const u = createUpdater(d)
    await u.installUpdate()
    expect(u.getStatus().kind).toBe('idle')
    expect(d.isEngineRunning).not.toHaveBeenCalled()
  })
})
