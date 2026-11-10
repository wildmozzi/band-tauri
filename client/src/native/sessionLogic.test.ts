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
  it('시작하면 브로드캐스트하고, 그때 알던 연주자에게는 등장 알림으로 다시 보내지 않는다', () => {
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
    expect(m.reannounce(['B'])).toEqual([])
  })

  it('descriptor 수신 시 add_peer + to 회신, 같은 descriptor 중복은 무시', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    expect(kinds(m.descriptorReceived('B', desc(2)))).toEqual(['addPeer:B', 'sendTo:B'])
    expect(m.descriptorReceived('B', desc(2))).toEqual([])
    expect(m.peers).toEqual(['B'])
  })

  it('처음 받은 descriptor에는 이미 브로드캐스트로 보냈어도 회신한다 (상대가 내 것을 놓쳤을 수 있음)', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), ['B'])
    expect(kinds(m.descriptorReceived('B', desc(2)))).toEqual(['addPeer:B', 'sendTo:B'])
    // 그 뒤 같은 descriptor 중복에는 회신하지 않는다
    expect(m.descriptorReceived('B', desc(2))).toEqual([])
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
    expect(kinds(m.descriptorReceived('B', desc(4)))).toEqual(['addPeer:B', 'sendTo:B'])
  })

  it('엔진에 등록하지 않은 피어의 이탈은 보냄 표시만 지운다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), ['B'])
    expect(m.peerGone('B')).toEqual([])
    expect(m.hasSentTo('B')).toBe(false)
  })

  it('M2: 정원 초과 피어는 대기했다가 자리가 나면 등록하고 회신한다', () => {
    const m = new NativeSignalMachine(6)
    m.started('A', desc(1), [])
    for (let i = 0; i < 5; i++) m.descriptorReceived(`P${i}`, desc(10 + i))
    expect(m.descriptorReceived('X', desc(99))).toEqual([{ kind: 'rejectPeer', peerId: 'X', reason: 'room-full' }])
    expect(m.peers).toHaveLength(5)
    expect(m.waitingPeers).toEqual(['X'])
    // X가 재시작해 새 descriptor를 보내면 대기 descriptor 교체
    m.descriptorReceived('X', desc(100))
    expect(kinds(m.peerGone('P2'))).toEqual(['removePeer:P2', 'addPeer:X', 'sendTo:X'])
    const add = m.peerGone('nobody')
    expect(add).toEqual([])
    expect(m.peers).toContain('X')
    expect(m.waitingPeers).toEqual([])
  })

  it('M2: 대기 중인 피어가 나가면 대기열에서 빠진다', () => {
    const m = new NativeSignalMachine(2)
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    m.descriptorReceived('C', desc(3))
    expect(m.waitingPeers).toEqual(['C'])
    m.peerGone('C')
    expect(kinds(m.peerGone('B'))).toEqual(['removePeer:B'])
  })

  it('M1: add_peer 실패 후 같은 descriptor로 재시도, 성공하면 재시도 없음', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    m.addPeerFailed('B')
    expect(m.retryAddPeer('B')).toEqual([{ kind: 'addPeer', peerId: 'B', descriptor: desc(2) }])
    expect(m.retryAddPeer('B')).toEqual([])
    m.addPeerFailed('B')
    m.addPeerSucceeded('B')
    expect(m.retryAddPeer('B')).toEqual([])
    // 나간 피어는 재시도하지 않는다
    m.addPeerFailed('B')
    m.peerGone('B')
    expect(m.retryAddPeer('B')).toEqual([])
  })

  it('세션 재개: reannounce는 같은 descriptor를 다시 브로드캐스트한다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    expect(m.reannounce(['B', 'C'])).toEqual([{ kind: 'broadcast', descriptor: desc(1) }])
    expect(m.peers).toEqual(['B'])
    expect(m.performerAppeared('C')).toEqual([])
  })

  it('정지하면 상태를 비운다', () => {
    const m = new NativeSignalMachine()
    m.started('A', desc(1), [])
    m.descriptorReceived('B', desc(2))
    m.stopped()
    expect(m.running).toBe(false)
    expect(m.peers).toEqual([])
  })
})

type Msg = { to: string; from: string; descriptor: LocalDescriptor }

/** 서버 중계 흉내: 브로드캐스트는 그 시점 연주자 전원(연결된 사람만), to는 대상이 연결된 연주자일 때만 */
class Net {
  machines = new Map<string, NativeSignalMachine>()
  added = new Map<string, Map<string, number>>()
  performers = new Set<string>()
  offline = new Set<string>()
  queue: Msg[] = []
  messages = 0

  exec(self: string, actions: SessionAction[]) {
    for (const a of actions) {
      if (a.kind === 'broadcast') {
        for (const p of this.performers) if (p !== self) this.deliver({ to: p, from: self, descriptor: a.descriptor })
      } else if (a.kind === 'sendTo') {
        if (this.performers.has(a.peerId)) this.deliver({ to: a.peerId, from: self, descriptor: a.descriptor })
      } else if (a.kind === 'addPeer') {
        const m = this.added.get(self)!
        m.set(a.peerId, (m.get(a.peerId) ?? 0) + 1)
      } else if (a.kind === 'removePeer') {
        this.added.get(self)!.delete(a.peerId)
      }
    }
  }
  deliver(msg: Msg) {
    this.messages++
    if (this.offline.has(msg.to) || this.offline.has(msg.from)) return // 끊긴 동안 유실
    this.queue.push(msg)
  }
  drain() {
    while (this.queue.length) {
      const msg = this.queue.shift()!
      this.exec(msg.to, this.machines.get(msg.to)!.descriptorReceived(msg.from, msg.descriptor))
    }
  }
  join(id: string, ssrc: number, visibility: 'immediate' | 'delayed') {
    const m = new NativeSignalMachine()
    this.machines.set(id, m)
    this.added.set(id, new Map())
    const known = [...this.performers]
    this.performers.add(id)
    const notify = () => {
      for (const other of known) if (!this.offline.has(other)) this.exec(other, this.machines.get(other)!.performerAppeared(id))
    }
    if (visibility === 'immediate') notify()
    this.exec(id, m.started(id, desc(ssrc), known))
    if (visibility === 'delayed') {
      this.drain()
      notify()
    }
    this.drain()
  }
  fullMesh(ids: string[]) {
    for (const id of ids) {
      const peers = this.added.get(id)!
      expect([...peers.keys()].sort()).toEqual(ids.filter((x) => x !== id).sort())
      for (const count of peers.values()) expect(count).toBe(1)
    }
  }
}

describe('시그널링 시뮬레이션', () => {
  for (const visibility of ['immediate', 'delayed'] as const) {
    it(`6명이 순서대로 들어와도 풀메시가 완성된다 (${visibility})`, () => {
      const net = new Net()
      const ids = ['C', 'A', 'F', 'B', 'E', 'D']
      ids.forEach((id, i) => net.join(id, 100 + i, visibility))
      net.fullMesh(ids)
      // 쌍마다 최대 4개(브로드캐스트·등장 알림 각 1 + 첫 등록 회신 각 1) 이하로 수렴
      expect(net.messages).toBeLessThanOrEqual(15 * 4)
    })
  }

  it('세션 재개: 내가 끊긴 동안 시작한 연주자와도 reannounce 후 연결된다', () => {
    const net = new Net()
    net.join('A', 1, 'immediate')
    net.join('B', 2, 'immediate')
    // B의 WS가 끊김 (서버는 재개 유예 동안 B를 연주자로 유지)
    net.offline.add('B')
    net.join('X', 3, 'immediate')
    expect(net.added.get('X')!.has('B')).toBe(false)
    expect(net.added.get('B')!.has('X')).toBe(false)
    // 재개: B는 엔진을 유지한 채 descriptor 재전송
    net.offline.delete('B')
    net.exec('B', net.machines.get('B')!.reannounce(['A', 'X']))
    net.drain()
    net.fullMesh(['A', 'B', 'X'])
  })
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
