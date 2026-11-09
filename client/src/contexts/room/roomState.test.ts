import { describe, expect, it } from 'vitest'
import { mergeSelfPerformer, withPeer, type PeerInstrument } from './roomState'
import { makeChatId } from './useChat'

const p = (id: string, isHost = false): PeerInstrument => ({ peerId: id, instrument: 'guitar', nickname: id, isHost })

describe('mergeSelfPerformer (H3)', () => {
  const remote = { b: p('b'), c: p('c') }
  it('연주 중이면 나를 포함', () => {
    const m = mergeSelfPerformer(remote, { id: 'a', instrument: 'drums', running: true, nickname: 'me', isHost: true })
    expect(Object.keys(m).sort()).toEqual(['a', 'b', 'c'])
    expect(m.a).toEqual({ peerId: 'a', instrument: 'drums', nickname: 'me', isHost: true })
  })
  it('서버가 거부해 연주 세션이 내려가면 즉시 빠진다 (방송 담당 오판 방지)', () => {
    const m = mergeSelfPerformer(remote, { id: 'a', instrument: null, running: false, nickname: 'me', isHost: false })
    expect(Object.keys(m).sort()).toEqual(['b', 'c'])
  })
  it('시작 중(running=false)에는 포함하지 않는다', () => {
    expect('a' in mergeSelfPerformer(remote, { id: 'a', instrument: 'x', running: false, nickname: 'me', isHost: false })).toBe(false)
  })
  it('원격 목록에 내 id가 섞여 와도 세션 상태가 기준', () => {
    const m = mergeSelfPerformer({ ...remote, a: p('a') }, { id: 'a', instrument: null, running: false, nickname: 'me', isHost: false })
    expect('a' in m).toBe(false)
  })
})

describe('withPeer (H1 클라이언트 방어)', () => {
  it('모르는 id를 추가하고 나/중복/빈 값은 무시', () => {
    expect(withPeer(['b'], 'c', 'a')).toEqual(['b', 'c'])
    const same = ['b']
    expect(withPeer(same, 'b', 'a')).toBe(same)
    expect(withPeer(same, 'a', 'a')).toBe(same)
    expect(withPeer(same, null, 'a')).toBe(same)
  })
})

describe('makeChatId (L10)', () => {
  it('같은 발신자·같은 ms여도 id가 다르다', () => {
    const ids = new Set([makeChatId('u', 1000), makeChatId('u', 1000), makeChatId('u', 1000)])
    expect(ids.size).toBe(3)
  })
})
