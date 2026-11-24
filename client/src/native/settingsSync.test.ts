import { describe, expect, it, vi } from 'vitest'
import { SettingsSaveGate, TestMonitorControl } from './settingsSync'

describe('SettingsSaveGate (M7, L1)', () => {
  it('그 사용자의 DB 값을 읽기 전에는 저장하지 않는다', () => {
    const g = new SettingsSaveGate()
    g.setUser('A')
    g.markEdited()
    expect(g.shouldSave('A')).toBe(false)
    g.markLoaded('A')
    expect(g.shouldSave('A')).toBe(true)
  })

  it('M7: A에서 바꾼 설정이 B로 전환된 뒤 B 프로필에 저장되지 않는다', () => {
    const g = new SettingsSaveGate()
    g.setUser('A')
    g.markLoaded('A')
    g.markEdited()
    expect(g.shouldSave('A')).toBe(true)
    expect(g.setUser(null)).toBe(true)
    expect(g.setUser('B')).toBe(true)
    expect(g.shouldSave('B')).toBe(false) // dirty/loaded 초기화
    g.markLoaded('B')
    expect(g.shouldSave('B')).toBe(false) // B는 아직 아무것도 안 바꿈
    g.markEdited()
    expect(g.shouldSave('B')).toBe(true)
    expect(g.shouldSave('A')).toBe(false)
  })

  it('L1: 취소된 로드(StrictMode 이중 effect)는 loaded로 표시되지 않아 다시 읽는다', () => {
    const g = new SettingsSaveGate()
    g.setUser('A')
    expect(g.shouldLoad('A')).toBe(true)
    // 첫 effect의 로드가 취소됨 → markLoaded 호출 안 함
    expect(g.setUser('A')).toBe(false) // 두 번째 effect: 같은 사용자
    expect(g.shouldLoad('A')).toBe(true)
    g.markLoaded('A')
    expect(g.shouldLoad('A')).toBe(false)
  })

  it('다른 사용자의 늦은 로드 완료는 무시', () => {
    const g = new SettingsSaveGate()
    g.setUser('A')
    g.setUser('B')
    g.markLoaded('A')
    expect(g.shouldLoad('B')).toBe(true)
    g.markEdited()
    expect(g.shouldSave('B')).toBe(false)
  })
})

describe('TestMonitorControl (L8)', () => {
  it('시작 중에 stop이 오면 시작이 끝난 뒤 정지한다', async () => {
    const calls: string[] = []
    let finish!: () => void
    const ctl = new TestMonitorControl({
      start: () => new Promise<void>((r) => { calls.push('start'); finish = r }),
      stop: async () => { calls.push('stop') },
    })
    const s = ctl.start()
    expect(ctl.isStarting).toBe(true)
    const st = ctl.stop()
    finish()
    await Promise.all([s, st])
    expect(calls).toEqual(['start', 'stop'])
    expect(ctl.isActive).toBe(false)
  })

  it('시작하지 않았으면 stop은 엔진을 건드리지 않는다', async () => {
    const calls: string[] = []
    const ctl = new TestMonitorControl({ start: async () => { calls.push('start') }, stop: async () => { calls.push('stop') } })
    await ctl.stop()
    expect(calls).toEqual([])
  })

  it('시작 실패면 active가 아니고 stop도 호출하지 않는다', async () => {
    const calls: string[] = []
    const ctl = new TestMonitorControl({ start: async () => { throw new Error('busy') }, stop: async () => { calls.push('stop') } })
    await expect(ctl.start()).rejects.toThrow('busy')
    await ctl.stop()
    expect(ctl.isActive).toBe(false)
    expect(calls).toEqual([])
  })

  describe('restart — 드라이버 리셋 뒤 다시 열기 (#261 재리뷰 L4)', () => {
    const noSleep = async () => {}

    it('대기 중 바깥 stop: 대기가 끝나길 기다리지 않고 바로 그만두고, 곧바로 다시 시작할 수 있다 (#263 리뷰 L-1)', async () => {
      let starts = 0
      let stops = 0
      const ctl = new TestMonitorControl({ start: async () => { starts++ }, stop: async () => { stops++ } })
      await ctl.start()
      let slept = 0
      const never = () => { slept++; return new Promise<void>(() => {}) } // 끝나지 않는 대기
      const r = ctl.restart([1, 1], never)
      await vi.waitFor(() => expect(slept).toBe(1)) // 닫고 첫 대기에 들어갔다
      expect(ctl.isRestarting).toBe(true)
      await ctl.stop()
      expect(ctl.isRestarting).toBe(false) // 그만둔 restart는 "다시 여는 중"이 아니다
      expect(await r).toBe('cancelled')
      expect(slept).toBe(1) // 다음 대기로 넘어가지 않았다
      expect(starts).toBe(1)
      // 사용자가 곧바로 다시 켠다 → 새 리셋도 정상으로 다시 연다
      await ctl.start()
      expect(ctl.isActive).toBe(true)
      expect(await ctl.restart([1], async () => {})).toBe('restarted')
      expect(starts).toBe(3)
      expect(stops).toBe(2) // 첫 restart 앞 정지 + 두 번째 restart 앞 정지(바깥 stop 때는 이미 꺼져 있었다)
      expect(ctl.isRestarting).toBe(false)
    })

        it('드라이버가 잠깐 열리지 않아도 여러 번 시도해 다시 켠다', async () => {
      const calls: string[] = []
      let failures = 0
      const ctl = new TestMonitorControl({
        start: async () => {
          calls.push('start')
          if (failures > 0) {
            failures--
            throw new Error('ASIO 장치를 찾을 수 없습니다')
          }
        },
        stop: async () => { calls.push('stop') },
      })
      await ctl.start()
      failures = 2
      const slept: number[] = []
      expect(await ctl.restart([1, 2, 3, 4], async (ms) => { slept.push(ms) })).toBe('restarted')
      expect(calls).toEqual(['start', 'stop', 'start', 'start', 'start'])
      expect(slept).toEqual([1, 2, 3])
      expect(ctl.isActive).toBe(true)
      expect(ctl.isRestarting).toBe(false)
    })

    it('전부 실패하면 failed이고 꺼진 상태로 남는다', async () => {
      let fail = false
      const ctl = new TestMonitorControl({ start: async () => { if (fail) throw new Error('x') }, stop: async () => {} })
      await ctl.start()
      fail = true
      expect(await ctl.restart([1, 1, 1], noSleep)).toBe('failed')
      expect(ctl.isActive).toBe(false)
    })

    it('다시 여는 사이 바깥에서 stop하면 그만둔다(다시 켜지 않는다)', async () => {
      const calls: string[] = []
      const ctl = new TestMonitorControl({ start: async () => { calls.push('start') }, stop: async () => { calls.push('stop') } })
      await ctl.start()
      const r = ctl.restart([1, 1], async () => { await ctl.stop() })
      expect(await r).toBe('cancelled')
      expect(calls).toEqual(['start', 'stop'])
      expect(ctl.isActive).toBe(false)
    })

    it('켜져 있지 않거나 이미 다시 여는 중이면 아무것도 하지 않는다', async () => {
      const calls: string[] = []
      const ctl = new TestMonitorControl({ start: async () => { calls.push('start') }, stop: async () => { calls.push('stop') } })
      expect(await ctl.restart([1], noSleep)).toBe('cancelled')
      await ctl.start()
      let release!: () => void
      const first = ctl.restart([1], () => new Promise<void>((r) => { release = r }))
      await Promise.resolve()
      await Promise.resolve()
      expect(ctl.isRestarting).toBe(true)
      expect(await ctl.restart([1], noSleep)).toBe('cancelled')
      release()
      expect(await first).toBe('restarted')
      expect(calls).toEqual(['start', 'stop', 'start'])
    })
  })

  it('중복 start는 한 번만', async () => {
    let n = 0
    const ctl = new TestMonitorControl({ start: async () => { n++ }, stop: async () => {} })
    await Promise.all([ctl.start(), ctl.start()])
    await ctl.start()
    expect(n).toBe(1)
  })
})
