/**
 * 데스크톱 전용 엔진 런타임 묶음 — 연주 세션 본체(PerformSession)와 관람 방송(mixBridge).
 * runtimeLoader가 데스크톱 앱에서만 동적 import한다(앱 첫 그리기 전에 미리 로드). 웹 관람 첫 묶음에는 실리지 않는다.
 * 이 파일을 정적으로 import하지 말 것 — runtimeLoader로만 쓴다.
 */
import { engineAddPeer, engineRemovePeer, engineSetInputMuted, engineSetMetronome, engineSetPeerGain, engineSetRelay, engineStart, engineStats, engineStop } from './engineApi'
import { reapplyLoopbackOffsetInBackground } from './loopbackOffsetStore'
import { MixBroadcaster, MixSource } from './mixBridge'
import { PerformSession, type EngineBackend, type SessionEnv } from './performSession'

export const tauriEngineBackend: EngineBackend = {
  start: async (config, peerId) => {
    const descriptor = await engineStart(config, peerId)
    // 앱 재시작 뒤에도 같은 장치 구성이면 메트로놈 출력 지연 보정값을 다시 넣는다(P2-3c)
    reapplyLoopbackOffsetInBackground(config)
    return descriptor
  },
  stop: engineStop,
  addPeer: engineAddPeer,
  setRelay: engineSetRelay,
  removePeer: engineRemovePeer,
  setPeerGain: engineSetPeerGain,
  setInputMuted: engineSetInputMuted,
  setMetronome: engineSetMetronome,
  stats: engineStats,
}

export function createPerformSession(env: SessionEnv): PerformSession {
  return new PerformSession(tauriEngineBackend, env)
}

export function createMixBridge(send: (payload: Record<string, unknown> & { type: string }) => void, onViewerCountChange: (connected: number) => void) {
  const mixSource = new MixSource()
  return { mixSource, broadcaster: new MixBroadcaster(mixSource, send, onViewerCountChange) }
}
