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

  /**
   * 크기·속도를 바꾼다(관람 중계 버킷 — 방 인원에 따라). 커지면 늘어난 만큼 토큰을 바로 더한다(새로 들어온 사람 몫의 버스트),
   * 작아지면 남은 토큰을 새 크기로 자른다. 같은 값이면 아무것도 하지 않는다.
   */
  resize(capacity: number, perSec: number): void {
    if (capacity === this.capacity && perSec === this.perSec) return
    this.take(0) // 지금까지의 충전을 옛 속도로 반영
    if (capacity > this.capacity) this.tokens += capacity - this.capacity
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
