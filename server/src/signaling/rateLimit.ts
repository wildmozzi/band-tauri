export class TokenBucket {
  private tokens: number
  private last = Date.now()
  constructor(private capacity: number, private perSec: number) {
    this.tokens = capacity
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
