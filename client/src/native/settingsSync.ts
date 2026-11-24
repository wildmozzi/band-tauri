/**
 * 오디오 설정 저장/테스트 보조 (React 비의존, 테스트 대상).
 */

/**
 * profiles.audio_settings 저장 게이트 (M7, L1).
 * - 사용자가 바뀌면 dirty/loaded를 초기화한다 → 이전 사용자의 설정을 새 사용자 프로필에 쓰지 않는다.
 * - 그 사용자의 DB 값을 읽기 전에는 저장하지 않는다.
 * - 로드가 취소되면(StrictMode 이중 effect 등) loaded로 표시하지 않으므로 다음 effect에서 다시 읽는다.
 */
export class SettingsSaveGate {
  private userId: string | null = null
  private loadedFor: string | null = null
  private dirty = false

  /** 현재 사용자 갱신. 바뀌었으면 true */
  setUser(userId: string | null): boolean {
    if (userId === this.userId) return false
    this.userId = userId
    this.loadedFor = null
    this.dirty = false
    return true
  }

  shouldLoad(userId: string | null): boolean {
    return !!userId && userId === this.userId && this.loadedFor !== userId
  }

  /** DB 읽기 완료(값이 없어도). 취소된 로드에서는 부르지 않는다 */
  markLoaded(userId: string): void {
    if (userId === this.userId) this.loadedFor = userId
  }

  /** 사용자가 설정을 바꿈 */
  markEdited(): void {
    this.dirty = true
  }

  shouldSave(userId: string | null): boolean {
    return !!userId && userId === this.userId && this.loadedFor === userId && this.dirty
  }
}

/**
 * 테스트 모니터(engine_test_start/stop) 제어 (L8).
 * 시작 중에 stop이 오면 시작이 끝날 때까지 기다렸다가 정지한다 → 합주 엔진 시작과 테스트가 겹치지 않는다.
 */
export class TestMonitorControl {
  private active = false
  private pending: Promise<void> | null = null
  private restarting = false
  /** 바깥(사용자·합주 시작)에서 stop할 때마다 오른다 — 진행 중인 restart가 그 뒤로 다시 켜지 않게 */
  private stopGen = 0

  constructor(private readonly api: { start(): Promise<unknown>; stop(): Promise<unknown> }) {}

  get isActive(): boolean {
    return this.active
  }

  get isStarting(): boolean {
    return this.pending !== null
  }

  /** 드라이버 리셋으로 다시 여는 중(restart) */
  get isRestarting(): boolean {
    return this.restarting
  }

  async start(): Promise<void> {
    if (this.active || this.pending) return
    this.pending = this.api.start().then(() => {
      this.active = true
    })
    try {
      await this.pending
    } finally {
      this.pending = null
    }
  }

  async stop(): Promise<void> {
    this.stopGen++
    await this.stopInner()
  }

  private async stopInner(): Promise<void> {
    if (this.pending) await this.pending.catch(() => {})
    if (!this.active) return
    this.active = false
    await this.api.stop()
  }

  /**
   * 드라이버 리셋 요청(엔진 `deviceReset`) 뒤 테스트 모니터를 같은 설정으로 다시 연다(#261 재리뷰 L4). 합주의 리셋 재시작과 같은 규칙:
   * 드라이버가 재설정하느라 잠깐 열리지 않을 수 있어 대기(delaysMs)를 두고 여러 번 시도하고, 다른 장치로 바꾸지 않는다.
   * 그사이 바깥에서 stop하면(사용자가 끔, 합주 시작) 그만둔다. 다시 켜졌으면 'restarted', 전부 실패면 'failed', 그만뒀으면 'cancelled'.
   */
  async restart(delaysMs: number[], sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms))): Promise<'restarted' | 'failed' | 'cancelled'> {
    if (!this.active || this.restarting) return 'cancelled'
    this.restarting = true
    const gen = this.stopGen
    try {
      await this.stopInner().catch(() => {})
      for (const d of delaysMs) {
        await sleep(d)
        if (gen !== this.stopGen) return 'cancelled'
        try {
          await this.start()
        } catch {
          continue // 드라이버가 아직 재설정 중 — 다음 시도
        }
        // 시작하는 사이 바깥 stop이 왔으면 그 stop이 시작이 끝난 뒤 끈다(L8)
        if (gen !== this.stopGen) return 'cancelled'
        if (this.active) return 'restarted'
      }
      return gen !== this.stopGen ? 'cancelled' : 'failed'
    } finally {
      this.restarting = false
    }
  }
}
