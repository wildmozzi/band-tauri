import { describe, expect, it } from 'vitest'
import type { LocalDescriptor } from './engineApi'
import {
  NativeSignalMachine,
  broadcastTransition,
  computeViewers,
  isValidDescriptor,
  selectBroadcaster,
  type SessionAction,
} from './sessionLogic'

const desc = (ssrc: number): LocalDescriptor => ({
  ssrc,
  key: `key-${ssrc}`,
  candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }, { kind: 'public', addr: '203.0.113.5:61000' }],
})

const kinds = (actions: SessionAction[]) => actions.map((a) => a.kind + ('peerId' in a ? `:${a.peerId}` : ''))

describe('NativeSignalMachine', () => {
  it('시작하면 브로드캐스트하고, 그때 알던 연주자에게는 다시 보내지 않는다', () => {
    const m = new NativeSignalMachine()
    expect(kinds(m.started('A', desc(1), ['B', 'A']))).toEqual(['broadcast'])
    expect(m.hasSentTo('B')).toBe(true)
    expect(m.hasSentTo('A')).toBe(false)
    expect(m.performerAppeared('B')).toEqual([])
  })

  it('엔진이 안 돌면 아무것도 하지 않는다', () => {
    const m = new NativeSignalMachine()
    expect(m.descriptorReceived('B', desc(2))).toEqual([])
    expect(m.performerAppeared('B')).toEqual([])
  })

  it('descriptor 수신 시 add_peer + 아직 안 보냈으면 to로 회신(한 번만)', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    expect(kinds(m.descriptorReceived('B', desc(2)))).toEqual(['addPeer:B', 'sendTo:B'])
    // 같은 descriptor 중복 수신: 아무것도 안 함
    expect(m.descriptorReceived('B', desc(2))).toEqual([])
    expect(m.peers).toEqual(['B'])
  })

  it('이미 브로드캐스트로 보낸 피어의 descriptor에는 회신하지 않는다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), ['B'])
    expect(kinds(m.descriptorReceived('B', desc(2)))).toEqual(['addPeer:B'])
  })

  it('내 id로 온 것은 무시한다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    expect(m.descriptorReceived('A', desc(1))).toEqual([])
    expect(m.performerAppeared('A')).toEqual([])
  })

  it('상대 엔진 재시작(ssrc 변경) → remove 후 add, 다시 회신', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    expect(kinds(m.descriptorReceived('B', desc(3)))).toEqual(['removePeer:B', 'addPeer:B', 'sendTo:B'])
  })

  it('이탈하면 remove_peer, 다시 나타나면 다시 보낸다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    expect(kinds(m.peerGone('B'))).toEqual(['removePeer:B'])
    expect(m.peerGone('B')).toEqual([])
    expect(kinds(m.performerAppeared('B'))).toEqual(['sendTo:B'])
    expect(kinds(m.descriptorReceived('B', desc(4)))).toEqual(['addPeer:B'])
  })

  it('엔진에 등록하지 않은 피어의 이탈은 보냄 표시만 지운다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), ['B'])
    expect(m.peerGone('B')).toEqual([])
    expect(m.hasSentTo('B')).toBe(false)
  })

  it('최대 6명(원격 5명)을 넘으면 거부', () => {
    const m = new NativeSignalMachine(6)
    m.started('A', desc(1), [])
    for (let i = 0; i < 5; i++) m.descriptorReceived(`P${i}`, desc(10 + i))
    expect(m.descriptorReceived('P5', desc(99))).toEqual([{ kind: 'rejectPeer', peerId: 'P5', reason: 'room-full' }])
    expect(m.peers).toHaveLength(5)
  })

  it('정지하면 상태를 비우고 add_peer 실패 시 다음 descriptor로 재시도한다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    m.addPeerFailed('B')
    expect(kinds(m.descriptorReceived('B', desc(2)))).toEqual(['addPeer:B'])
    m.stopped()
    expect(m.running).toBe(false)
    expect(m.peers).toEqual([])
  })
})

/**
 * 서버 중계를 흉내 낸 시뮬레이션: 연주자들이 임의 순서로 시작해도 모두 서로를 정확히 한 번씩 등록하고 메시지가 수렴하는지.
 * 서버 규칙: 브로드캐스트는 "그 시점 연주자" 전원에게, to는 대상이 연주자일 때만 중계.
 */
function simulate(order: string[], performerVisibility: 'immediate' | 'delayed') {
  const machines = new Map<string, NativeSignalMachine>()
  const added = new Map<string, Map<string, number>>()
  const performers = new Set<string>()
  const queue: { to: string; from: string; descriptor: LocalDescriptor }[] = []
  let messages = 0

  const exec = (self: string, actions: SessionAction[]) => {
    for (const a of actions) {
      if (a.kind === 'broadcast') {
        for (const p of performers) if (p !== self) { queue.push({ to: p, from: self, descriptor: a.descriptor }); messages++ }
      } else if (a.kind === 'sendTo') {
        if (performers.has(a.peerId)) { queue.push({ to: a.peerId, from: self, descriptor: a.descriptor }); messages++ }
      } else if (a.kind === 'addPeer') {
        const m = added.get(self)!
        m.set(a.peerId, (m.get(a.peerId) ?? 0) + 1)
      } else if (a.kind === 'removePeer') {
        added.get(self)!.delete(a.peerId)
      }
    }
  }
  const drain = () => {
    while (queue.length) {
      const msg = queue.shift()!
      exec(msg.to, machines.get(msg.to)!.descriptorReceived(msg.from, msg.descriptor))
    }
  }

  order.forEach((id, i) => {
    const m = new NativeSignalMachine()
    machines.set(id, m)
    added.set(id, new Map())
    const known = [...performers]
    // 엔진 시작 → start-performing (서버가 연주자로 등록) → announce
    performers.add(id)
    if (performerVisibility === 'immediate') {
      for (const other of known) exec(other, machines.get(other)!.performerAppeared(id))
    }
    exec(id, m.started(id, desc(100 + i), known))
    if (performerVisibility === 'delayed') {
      drain()
      for (const other of known) exec(other, machines.get(other)!.performerAppeared(id))
    }
    drain()
  })
  return { added, messages }
}

describe('시그널링 시뮬레이션', () => {
  for (const visibility of ['immediate', 'delayed'] as const) {
    it(`6명이 순서대로 들어와도 풀메시가 완성된다 (${visibility})`, () => {
      const ids = ['C', 'A', 'F', 'B', 'E', 'D']
      const { added, messages } = simulate(ids, visibility)
      for (const id of ids) {
        const peers = added.get(id)!
        expect([...peers.keys()].sort()).toEqual(ids.filter((x) => x !== id).sort())
        for (const count of peers.values()) expect(count).toBe(1)
      }
      // 쌍마다 최대 3개(브로드캐스트 + 회신 + 등장 알림) 이하로 수렴
      expect(messages).toBeLessThanOrEqual(15 * 3)
    })
  }
})

describe('selectBroadcaster', () => {
  it('연주자가 없으면 null', () => {
    expect(selectBroadcaster([])).toBeNull()
  })
  it('호스트가 연주자면 호스트', () => {
    expect(selectBroadcaster([{ id: 'a', isHost: false }, { id: 'z', isHost: true }])).toBe('z')
  })
  it('호스트가 없으면 id 사전순 최소', () => {
    expect(selectBroadcaster([{ id: 'm', isHost: false }, { id: 'b', isHost: false }, { id: 'x', isHost: false }])).toBe('b')
  })
  it('입력 순서와 무관하게 같은 결과', () => {
    const a = [{ id: 'q', isHost: false }, { id: 'c', isHost: false }, { id: 'k', isHost: false }]
    expect(selectBroadcaster(a)).toBe(selectBroadcaster([...a].reverse()))
  })
  it('호스트가 연주를 멈추면 사전순 최소로 인계', () => {
    const before = [{ id: 'h', isHost: true }, { id: 'b', isHost: false }]
    const after = [{ id: 'b', isHost: false }]
    expect(selectBroadcaster(before)).toBe('h')
    expect(selectBroadcaster(after)).toBe('b')
    expect(broadcastTransition('h', 'b', 'b')).toBe('start')
    expect(broadcastTransition('h', 'b', 'h')).toBe('stop')
    expect(broadcastTransition('b', 'b', 'b')).toBe('keep')
    expect(broadcastTransition('h', 'b', 'x')).toBe('none')
  })
})

describe('computeViewers', () => {
  it('연주자와 나를 뺀 참여자', () => {
    expect(computeViewers(['a', 'b', 'c', 'd', 'b'], ['b', 'me'], 'me')).toEqual(['a', 'c', 'd'])
  })
})

describe('isValidDescriptor', () => {
  it('정상 descriptor 허용', () => {
    expect(isValidDescriptor(desc(1))).toBe(true)
  })
  it('잘못된 값 거부', () => {
    expect(isValidDescriptor(null)).toBe(false)
    expect(isValidDescriptor({ ssrc: -1, key: 'k', candidates: [{ kind: 'lan', addr: '1.2.3.4:5' }] })).toBe(false)
    expect(isValidDescriptor({ ssrc: 1, key: '', candidates: [{ kind: 'lan', addr: '1.2.3.4:5' }] })).toBe(false)
    expect(isValidDescriptor({ ssrc: 1, key: 'k', candidates: [] })).toBe(false)
    expect(isValidDescriptor({ ssrc: 1, key: 'k', candidates: [{ kind: 'turn', addr: '1.2.3.4:5' }] })).toBe(false)
    expect(isValidDescriptor({ ssrc: 1, key: 'k', candidates: [{ kind: 'lan', addr: 'evil.com:5' }] })).toBe(false)
  })
})
