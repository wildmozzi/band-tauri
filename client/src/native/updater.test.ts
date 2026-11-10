import { describe, expect, it, vi } from 'vitest'
import { createUpdater, type DownloadEvent, type UpdateHandle, type UpdaterDeps } from './updater'

interface FakeUpdate extends UpdateHandle {
  downloads: number
  installs: number
  /** download 도중 실행할 훅 (예: 다운로드 중 엔진 시작) */
  duringDownload?: () => void
}

function fakeUpdate(version = '0.2.0', events: DownloadEvent[] = []): FakeUpdate {
  const u: FakeUpdate = {
    version,
    body: '변경 사항',
    downloads: 0,
    installs: 0,
    async download(onEvent) {
      events.forEach((e) => onEvent?.(e))
      u.duringDownload?.()
      u.downloads++
    },
    async install() {
      u.installs++
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

  it('엔진이 멈춰 있으면 available → 다운로드 진행률 → 설치 → 재시작', async () => {
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
    expect(u.getStatus()).toEqual({ kind: 'available', version: '0.2.0', notes: '변경 사항', ready: false })

    await u.installUpdate()
    expect(update.downloads).toBe(1)
    expect(update.installs).toBe(1)
    expect(d.relaunch).toHaveBeenCalledOnce()
    expect(seen).toEqual(['available', 'dl:0/null', 'dl:0/200', 'dl:120/200', 'dl:200/200', 'installing'])
  })

  it('연주 중이면 다운로드도 설치도 하지 않고 알림만 한다 (deferred)', async () => {
    let running = true
    const update = fakeUpdate()
    const d = deps({ check: vi.fn(async () => update), isEngineRunning: vi.fn(async () => running) })
    const u = createUpdater(d)

    await u.checkForUpdate({ silent: true })
    expect(u.getStatus().kind).toBe('deferred')

    await u.installUpdate()
    expect(u.getStatus().kind).toBe('deferred')
    expect(update.downloads).toBe(0)
    expect(update.installs).toBe(0)

    running = false
    await u.installUpdate()
    expect(update.installs).toBe(1)
    expect(d.relaunch).toHaveBeenCalledOnce()
  })

  it('다운로드 중 엔진이 시작되면 install을 호출하지 않고, 받은 패키지를 유지한다 (M1)', async () => {
    let running = false
    const update = fakeUpdate()
    update.duringDownload = () => {
      running = true
    }
    const d = deps({ check: vi.fn(async () => update), isEngineRunning: vi.fn(async () => running) })
    const u = createUpdater(d)

    await u.checkForUpdate()
    await u.installUpdate()
    expect(update.downloads).toBe(1)
    expect(update.installs).toBe(0)
    expect(d.relaunch).not.toHaveBeenCalled()
    expect(u.getStatus()).toMatchObject({ kind: 'deferred', ready: true })

    // 연주 종료 후: 다시 받지 않고 바로 설치
    running = false
    await u.installUpdate()
    expect(update.downloads).toBe(1)
    expect(update.installs).toBe(1)
  })

  it('같은 버전을 다시 확인해도 받아 둔 패키지를 버리지 않는다', async () => {
    let running = false
    const first = fakeUpdate('0.2.0')
    first.duringDownload = () => {
      running = true
    }
    const second = fakeUpdate('0.2.0')
    const check = vi.fn<UpdaterDeps['check']>().mockResolvedValueOnce(first).mockResolvedValueOnce(second)
    const u = createUpdater(deps({ check, isEngineRunning: vi.fn(async () => running) }))

    await u.checkForUpdate()
    await u.installUpdate() // 다운로드 후 보류
    running = false
    await u.checkForUpdate()
    expect(u.getStatus()).toMatchObject({ kind: 'available', ready: true })
    await u.installUpdate()
    expect(first.installs).toBe(1)
    expect(second.downloads).toBe(0)
  })

  it('방에 있는 동안은 엔진 시작 전이라도 보류, 방을 나가면 다시 알린다 (L4)', async () => {
    const update = fakeUpdate()
    const u = createUpdater(deps({ check: vi.fn(async () => update) }))

    u.setInRoom(true)
    await u.checkForUpdate({ silent: true })
    expect(u.getStatus().kind).toBe('deferred')
    await u.installUpdate()
    expect(update.downloads).toBe(0)

    u.dismiss() // "나중에"
    expect(u.getStatus().kind).toBe('idle')
    u.setInRoom(false)
    expect(u.getStatus()).toMatchObject({ kind: 'available', version: '0.2.0' })

    // available 상태에서 방에 들어가면 deferred로 바뀐다
    u.setInRoom(true)
    expect(u.getStatus().kind).toBe('deferred')
  })

  it('다운로드 실패와 설치 실패는 각각 error로 알린다', async () => {
    const bad = fakeUpdate()
    bad.download = async () => Promise.reject('네트워크 끊김')
    const u1 = createUpdater(deps({ check: vi.fn(async () => bad) }))
    await u1.checkForUpdate()
    await u1.installUpdate()
    expect(u1.getStatus()).toEqual({ kind: 'error', message: '업데이트 다운로드 실패: 네트워크 끊김' })

    const badInstall = fakeUpdate()
    badInstall.install = async () => Promise.reject('서명 검증 실패')
    const d = deps({ check: vi.fn(async () => badInstall) })
    const u2 = createUpdater(d)
    await u2.checkForUpdate()
    await u2.installUpdate()
    expect(u2.getStatus()).toEqual({ kind: 'error', message: '업데이트 설치 실패: 서명 검증 실패' })
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
