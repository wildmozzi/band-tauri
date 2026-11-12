/**
 * 관람자 브릿지 WebRTC 공통 설정 (송신: native/mixBridge.ts, 수신: features/room/state/useViewerAudio.ts).
 */

export const ICE_SERVERS: RTCIceServer[] = [{ urls: 'stun:stun.l.google.com:19302' }]

/**
 * Opus fmtp 조정. 방송 믹스는 스테레오이므로 stereo=1/sprop-stereo=1을 켜고,
 * 관람은 실시간 합주가 아니므로 FEC는 켜 둔다(손실 내성 우선).
 * Opus payload type은 브라우저마다 다를 수 있어 rtpmap에서 찾는다.
 */
export function tuneOpusSdp(sdp: string, opts: { stereo: boolean; maxBitrate?: number }): string {
  const match = sdp.match(/a=rtpmap:(\d+) opus\/48000(\/2)?/i)
  if (!match) return sdp
  const pt = match[1]
  const params: Record<string, string> = {
    stereo: opts.stereo ? '1' : '0',
    'sprop-stereo': opts.stereo ? '1' : '0',
    useinbandfec: '1',
  }
  if (opts.maxBitrate) params.maxaveragebitrate = String(opts.maxBitrate)

  const fmtpRe = new RegExp(`^a=fmtp:${pt} (.*)$`, 'm')
  const existing = sdp.match(fmtpRe)
  if (existing) {
    const kv = new Map<string, string>()
    for (const part of existing[1].split(';')) {
      const [k, v] = part.split('=').map((s) => s.trim())
      if (k) kv.set(k, v ?? '')
    }
    for (const [k, v] of Object.entries(params)) kv.set(k, v)
    const line = `a=fmtp:${pt} ${[...kv].map(([k, v]) => (v === '' ? k : `${k}=${v}`)).join(';')}`
    return sdp.replace(fmtpRe, line)
  }
  // fmtp 줄이 없으면 rtpmap 바로 뒤에 추가
  const rtpmapRe = new RegExp(`(a=rtpmap:${pt} [^\\r\\n]*)(\\r?\\n)`)
  const line = `a=fmtp:${pt} ${Object.entries(params).map(([k, v]) => `${k}=${v}`).join(';')}`
  return sdp.replace(rtpmapRe, `$1$2${line}$2`)
}
