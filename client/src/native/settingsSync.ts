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

  constructor(private readonly api: { start(): Promise<unknown>; stop(): Promise<unknown> }) {}

  get isActive(): boolean {
    return this.active
  }

  get isStarting(): boolean {
    return this.pending !== null
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
    if (this.pending) await this.pending.catch(() => {})
    if (!this.active) return
    this.active = false
    await this.api.stop()
  }
}
