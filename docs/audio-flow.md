# 합주실 오디오 전송 흐름

## 개요

이 문서는 Band 앱의 합주실에서 연주자의 악기 소리가 다른 참여자에게 전달되는 과정을 설명합니다.

---

## 전체 흐름도

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           【연주자 A - 보내는 쪽】                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1. 악기 (기타/베이스/키보드 등)                                              │
│         ↓                                                                   │
│  2. 오디오 인터페이스 (Scarlett, MOTU 등)                                     │
│         ↓  (USB/Thunderbolt)                                                │
│  3. 컴퓨터 OS 오디오 드라이버                                                 │
│         ↓                                                                   │
│  4. 브라우저 getUserMedia() API                                              │
│     - AudioSettingsContext에서 설정한 장치/설정 적용                          │
│     - deviceId, sampleRate(48kHz), channelCount                            │
│     - echoCancellation, noiseSuppression (악기는 OFF)                       │
│         ↓                                                                   │
│  5. MediaStream (localStream)                                               │
│         ↓                                                                   │
│  6. RTCPeerConnection.addTrack()                                            │
│     - WebRTC가 오디오를 Opus 코덱으로 인코딩                                   │
│         ↓                                                                   │
└─────────────────────────────────────────────────────────────────────────────┘
                              ↓ (인터넷)
┌─────────────────────────────────────────────────────────────────────────────┐
│                           【시그널링 서버】                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│  - WebSocket (SIGNALING_URL)                                                │
│  - SDP offer/answer 교환                                                     │
│  - ICE candidate 교환                                                        │
│  - STUN 서버: stun.l.google.com:19302                                       │
└─────────────────────────────────────────────────────────────────────────────┘
                              ↓ (P2P 연결 수립 후)
┌─────────────────────────────────────────────────────────────────────────────┐
│                           【연주자 B - 받는 쪽】                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  7. RTCPeerConnection.ontrack 이벤트                                         │
│     - 원격 MediaStream 수신                                                  │
│         ↓                                                                   │
│  8. remoteAudioMap에 저장                                                    │
│         ↓                                                                   │
│  9. Web Audio API (connectToWebAudio)                                       │
│     ┌──────────────────────────────────────────┐                           │
│     │  MediaStreamSource                        │                           │
│     │       ↓                                   │                           │
│     │  AnalyserNode (레벨 미터용)                │                           │
│     │       ↓                                   │                           │
│     │  GainNode (볼륨 조절)                      │                           │
│     │       ↓                                   │                           │
│     │  StereoPannerNode (패닝)                  │                           │
│     │       ↓                                   │                           │
│     │  AudioContext.destination                 │                           │
│     └──────────────────────────────────────────┘                           │
│         ↓                                                                   │
│  10. 스피커/헤드폰 출력                                                       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 단계별 상세 설명 (레이턴시 포함)

### 1단계: 악기 ⏱️ ~0ms
- 전기 기타, 베이스, 키보드, 전자 드럼 등
- 아날로그 신호 출력
- **레이턴시**: 거의 없음 (전기 신호 속도)
- **최적화 가능**: ✗ (이미 최소)

### 2단계: 오디오 인터페이스 ⏱️ 1-3ms
- 아날로그 → 디지털 변환 (ADC)
- 예: Focusrite Scarlett, MOTU M2, Universal Audio 등
- USB 또는 Thunderbolt로 컴퓨터에 연결
- **레이턴시**: 1-3ms (ADC 변환 + USB 전송)
- **최적화 가능**: △ (고급 장비일수록 낮음, Thunderbolt가 USB보다 빠름)

### 3단계: OS 오디오 드라이버 ⏱️ 3-10ms
- Windows: WASAPI, ASIO
- macOS: Core Audio
- 드라이버 버퍼 크기가 레이턴시에 영향
- **레이턴시**: 3-10ms (버퍼 크기에 따라 다름)
- **최적화 가능**: ○
  - ASIO 드라이버 사용 시 1-3ms까지 감소 가능
  - 버퍼 크기 줄이기 (64~128 samples 권장)
  - 단, 브라우저는 ASIO 직접 접근 불가

### 4단계: 브라우저 getUserMedia() API ⏱️ 5-20ms
```javascript
const constraints = {
  audio: {
    deviceId: { exact: selectedDeviceId },
    sampleRate: 48000,
    channelCount: 1,  // 모노 (기타, 베이스)
    echoCancellation: false,  // 악기용 OFF
    noiseSuppression: false,  // 악기용 OFF
    autoGainControl: false,   // 악기용 OFF
  }
}
const stream = await navigator.mediaDevices.getUserMedia(constraints)
```
- **레이턴시**: 5-20ms (브라우저 내부 버퍼링)
- **최적화 가능**: △ (제한적)
  - `echoCancellation`, `noiseSuppression` OFF로 약간 감소
  - 브라우저별로 다름 (Chrome이 가장 낮은 편)

### 5단계: MediaStream ⏱️ ~0ms
- 브라우저가 관리하는 오디오 스트림 객체
- `localStream` 상태로 저장
- **레이턴시**: 거의 없음 (메모리 참조)
- **최적화 가능**: ✗

### 6단계: WebRTC 인코딩 ⏱️ 10-30ms
```javascript
const pc = new RTCPeerConnection({ iceServers: ICE_SERVERS })
stream.getTracks().forEach(track => {
  pc.addTrack(track, stream)
})
```
- Opus 코덱으로 인코딩 (기본)
- 네트워크 상태에 따라 비트레이트 자동 조절
- **레이턴시**: 10-30ms (Opus 인코딩 + 패킷화)
- **최적화 가능**: △
  - Opus frame size 조절 (기본 20ms → 10ms로 변경 가능하나 제어 어려움)
  - CBR(Constant Bit Rate) 모드가 VBR보다 약간 빠름

### 7단계: 네트워크 전송 ⏱️ 20-100ms+
- P2P 연결로 데이터 전송
- **레이턴시**: 물리적 거리에 비례
  - 같은 LAN: 1-5ms
  - 같은 도시: 10-30ms
  - 같은 나라: 20-50ms
  - 해외: 100-200ms+
- **최적화 가능**: ✗ (물리적 한계, 빛의 속도)
  - TURN 서버 대신 직접 P2P 연결 시 약간 개선
  - 유선 연결이 Wi-Fi보다 안정적

### 8단계: Jitter Buffer (수신 버퍼) ⏱️ 20-50ms
- 네트워크 지터(떨림)를 흡수하기 위한 버퍼
- 패킷 순서 정렬 및 손실 보정
- **레이턴시**: 20-50ms (WebRTC 기본값)
- **최적화 가능**: ○
  - 버퍼 크기 줄이기 가능 (하지만 끊김 증가)
  - WebRTC에서는 직접 제어 어려움
  - 네트워크가 안정적일수록 작은 버퍼 가능

### 9단계: WebRTC 디코딩 ⏱️ 10-30ms
- Opus 디코딩
- **레이턴시**: 10-30ms
- **최적화 가능**: △ (인코딩과 동일)

### 10단계: Web Audio API 처리 ⏱️ 1-5ms
```javascript
const context = new AudioContext()
const source = context.createMediaStreamSource(stream)
const analyser = context.createAnalyser()
const gain = context.createGain()
const panner = context.createStereoPanner()

source.connect(analyser)
analyser.connect(gain)
gain.connect(panner)
panner.connect(context.destination)
```
- **레이턴시**: 1-5ms (AudioContext 버퍼)
- **최적화 가능**: ○
  - `latencyHint: 'interactive'` 옵션 사용
  - 노드 체인 최소화

### 11단계: 스피커/헤드폰 출력 ⏱️ ~0ms
- DAC 변환 및 출력
- **레이턴시**: 거의 없음
- **최적화 가능**: ✗

---

## 주요 코드 파일 위치

| 단계 | 파일 | 함수/위치 |
|------|------|----------|
| 4-5 | `client/src/contexts/AudioSettingsContext.tsx` | `testInput()`, `getUserMedia()` |
| 6 | `client/src/contexts/RoomContext.tsx:506` | `attachLocalTracks()` |
| 6 | `client/src/contexts/RoomContext.tsx:546` | `createPeerConnection()` |
| 7 | `client/src/contexts/RoomContext.tsx:556` | `pc.ontrack` 이벤트 |
| 9 | `client/src/contexts/RoomContext.tsx:242` | `connectToWebAudio()` |

---

## 현재 설정값

| 설정 | 값 | 비고 |
|------|-----|------|
| 샘플레이트 | 48kHz | 기본값 |
| 채널 | 모노/스테레오 | 악기별 다름 |
| 코덱 | Opus | WebRTC 기본 |
| STUN 서버 | stun.l.google.com:19302 | Google STUN |
| 에코 제거 | OFF | 악기용 |
| 노이즈 제거 | OFF | 악기용 |
| 자동 게인 | OFF | 악기용 |

---

## 레이턴시 발생 지점

```
[악기] → [오디오 인터페이스] → [OS 드라이버] → [브라우저] → [WebRTC 인코딩]
   ↓           ↓                   ↓              ↓              ↓
  0ms       1-3ms              3-10ms          5-20ms         10-30ms
                                                                 ↓
                                                            [네트워크]
                                                                 ↓
                                                            20-100ms+
                                                                 ↓
[스피커] ← [Web Audio] ← [WebRTC 디코딩] ← [수신 버퍼] ← [네트워크]
   ↓           ↓              ↓                ↓
  0ms        1-5ms         10-30ms          20-50ms
```

### 레이턴시 요약 테이블

| 구간 | 예상 지연 | 최적화 가능 | 비고 |
|------|----------|------------|------|
| 악기 | ~0ms | ✗ | 이미 최소 |
| 오디오 인터페이스 | 1-3ms | △ | 장비 의존 |
| OS 드라이버 | 3-10ms | ○ | ASIO 사용 시 1-3ms |
| 브라우저 캡처 | 5-20ms | △ | 제한적 |
| WebRTC 인코딩 | 10-30ms | △ | Opus 설정 |
| **네트워크 전송** | **20-100ms+** | **✗** | **물리적 한계** |
| Jitter Buffer | 20-50ms | ○ | 끊김 트레이드오프 |
| WebRTC 디코딩 | 10-30ms | △ | |
| Web Audio | 1-5ms | ○ | latencyHint 옵션 |
| 스피커 출력 | ~0ms | ✗ | |
| **총합** | **70-250ms+** | | |

### 총 예상 레이턴시
- **최적 환경 (같은 LAN, 유선)**: 50-80ms
- **좋은 환경 (같은 도시)**: 80-120ms
- **일반 환경 (다른 지역)**: 120-200ms
- **불량 환경 (해외/Wi-Fi)**: 200ms+

### 합주 가능 기준
| 레이턴시 | 합주 가능성 | 설명 |
|----------|------------|------|
| 0-20ms | 완벽 | 프로 스튜디오 수준, 실시간 합주 가능 |
| 20-50ms | 양호 | 대부분의 합주 가능, 빠른 곡은 어려움 |
| 50-100ms | 제한적 | 느린 템포 곡만 가능, 약간의 적응 필요 |
| 100-200ms | 어려움 | 리듬 맞추기 힘듦, 연습용으로만 사용 |
| 200ms+ | 불가능 | 합주 불가, 개별 연습 또는 녹음 공유만 가능 |

> ⚠️ **현실**: WebRTC로 인터넷을 통해 50ms 이하를 달성하는 것은 물리적으로 불가능합니다.
> 같은 LAN에서 유선 연결 시에만 50-80ms 수준이 가능합니다.

---

## 레이턴시 최적화 방안

### 현재 적용된 최적화
- ✅ 에코/노이즈 제거 OFF (악기용)
- ✅ 자동 게인 컨트롤 OFF
- ✅ P2P 직접 연결 (STUN 사용)

### 추가 가능한 최적화
- ⬜ AudioContext `latencyHint: 'interactive'` 적용
- ⬜ Jitter Buffer 크기 조절 (WebRTC 설정)
- ⬜ Opus 프레임 크기 최적화
- ⬜ 자체 TURN 서버 구축 (지연 감소)

### 근본적 해결책 (WebRTC 외)
- JAMULUS / JackTrip 같은 전용 프로토콜 사용
- 같은 네트워크(LAN) 전용 모드 개발
- 레이턴시 보상 기법 (클릭 트랙, 시각적 동기화)

---

## 관련 문서

- [WebRTC 공식 문서](https://webrtc.org/)
- [Web Audio API MDN](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API)
- [Opus 코덱](https://opus-codec.org/)
