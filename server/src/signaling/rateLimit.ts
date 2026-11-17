export class TokenBucket {
  private tokens: number
  private last = Date.now()
  constructor(private capacity: number, private perSec: number) {
    this.tokens = capacity
  }
  /** 가득 찼는지(오래 안 쓴 버킷 정리용) */
  isFull(): boolean {
    const now = Date.now()
    return this.tokens + ((now - this.last) / 1000) * this.perSec >= this.capacity
  }

  /** 이 버킷이 가졌던 최대 용량(resize의 토큰 더하기 기준, #215 리뷰 M1) */
  private peak = this.capacity

  /**
   * 크기·속도를 바꾼다(관람 중계 버킷 — 방 인원에 따라). 작아지면 남은 토큰을 새 크기로 자른다. 커질 때 토큰은 **이 버킷이 가졌던
   * 최대 용량을 처음 넘는 몫만** 바로 더한다 — 새 연결의 방송 담당이 관람자가 한꺼번에 붙을 때 버스트를 쓰게 하되, 방 인원을
   * 줄였다 늘리는 출입(leave/rejoin)으로 토큰을 거듭 받지 못하게(#215 리뷰 M1: 순환마다 370개). 그 밖에는 시간에 따라 채운다.
   * 같은 값이면 아무것도 하지 않는다.
   */
  resize(capacity: number, perSec: number): void {
    if (capacity === this.capacity && perSec === this.perSec) return
    this.take(0) // 지금까지의 충전을 옛 속도로 반영
    if (capacity > this.peak) {
      this.tokens += capacity - this.peak
      this.peak = capacity
    }
    this.capacity = capacity
    this.perSec = perSec
    this.tokens = Math.min(this.tokens, capacity)
  }

  /** n개(기본 1)를 꺼낸다. 모자라면 꺼내지 않고 false (바이트 단위 버킷은 n = 바이트 수) */
  take(n = 1): boolean {
    const now = Date.now()
    this.tokens = Math.min(this.capacity, this.tokens + ((now - this.last) / 1000) * this.perSec)
    this.last = now
    if (this.tokens < n) return false
    this.tokens -= n
    return true
  }
}
