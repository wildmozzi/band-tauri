# band(웹) → band-tauri 통합 보고서

- 기준: band `94d7cce`(2026-08-19), band-tauri `d3da96c`
- band-tauri 초기 커밋 `fe89259`(2026-01-12)은 band의 2026-01 초 상태에서 포크됨. band에서 포크 이후 생긴 커밋: `9178625`, `1107cc0`, `b6acc20`, `86a3726`, `c42c334`, `3b7ef2f`, `5ca9b18`, `e9fa7ad`, `428bee8`, `b446df3`, `94d7cce`. 이 중 뒤쪽 6개는 band-tauri 변경을 band로 역포팅한 것이다.
- 비교 범위: `client/`, `server/`, 루트 파일 (git 추적 파일 기준, `node_modules`/`dist`/`target` 제외)

## 이식함 (band → band-tauri)

| 파일 | 출처 커밋 | 내용 |
|---|---|---|
| `client/src/services/MusicXmlToMidi.ts` (신규) | 1107cc0, b6acc20 | MusicXML → MIDI 변환 |
| `client/src/services/SynthDrum.ts` (신규) | 1107cc0, b6acc20 | 합성 드럼 / 다중 악기 재생 |
| `client/src/components/ScoreDisplay.tsx` | 1107cc0 | 악보 페이지 표시, 마디 하이라이트 커서. band-tauri는 포크 이후 수정 없음 → band 최신본으로 교체. 새로 생긴 `no-explicit-any` 5곳에 OSMD 내부 API용 eslint-disable 주석 추가 |
| `client/src/pages/KaraokeVirtual.tsx` | 1107cc0 | PDF 업로드 → MusicXML → MIDI 흐름. 교체 후 `as any` 1곳 제거(`UploadResponse.warnings`로 타입 지정) |
| `client/src/services/MusicPlayer.ts` | 1107cc0 | 다중 악기 동시 재생(피아노/기타/드럼). 그대로 교체 |
| `client/src/components/FileUpload.tsx` | 1107cc0 | `.pdf` 업로드 허용. 그대로 교체 |
| `client/src/pages/Home.tsx` | 86a3726, c42c334 | "싱크룸" 문구 삭제 |
| `client/src/services/musicApi.ts` | 1107cc0, b6acc20 | `UploadResponse.fileType`에 `'pdf'`, `warnings` 추가 (나머지는 band-tauri 유지) |
| `server/src/services/omrService.ts` (신규) | 1107cc0 | Audiveris 기반 PDF → MusicXML(OMR) |
| `server/src/routes/music.ts` | 1107cc0 | `/upload-score`에 PDF(OMR) 분기, 실패 시 업로드 파일 정리, multer 저장 파일명 ASCII 정리(OMR 셸 명령 안전성). 음원분리/히스토리/인증 로직은 band-tauri 쪽 유지 |
| `client/src/contexts/RoomContext.tsx` | 9178625 | 비네이티브 부분 2곳만: ① 오디오 노드가 없을 때 레벨 미터 초기화 ② `applyPendingCandidates`에서 ICE가 이미 connected/completed/failed/closed면 대기 후보를 적용하지 않고 정리 |
| `client/src/pages/RoomDetail.tsx` | 9178625 | 연주자 목록 개별 음소거 버튼(WebRTC 스트림이 있을 때만, `hasWebRTCStream` 기준), 방장 승인 요청 알림을 참여 버튼 위로 이동 |
| `client/src/pages/RoomDetail.css` | 9178625 | `.performer-mute-btn` 스타일 |
| `docs/migrations/006_stem_separations.sql` (신규) | fd97e2b | 음원분리 히스토리 테이블. band-tauri 코드는 이미 이 테이블을 쓰는데 마이그레이션 파일이 없었음 |
| `server/.env.example` | — | band의 `huggingface` 프로바이더/`HF_SPACE_URL` 설명을 합치고 OMR(`AUDIVERIS_BAT`, `JAVA_HOME`), 결제(`TOSS_PAYMENTS_SECRET_KEY`), AMT 변수를 추가 |

## 유지함 (band-tauri가 더 최신이거나 Tauri 전용)

| 파일 | 이유 |
|---|---|
| `client/src/components/AudioSettings.tsx/.css`, `contexts/AudioSettingsContext.tsx` | band 쪽은 포크 이전 버전(마지막 변경 a1f4da9/11ebab9). band-tauri는 네이티브 드라이버 설정을 추가했다. 웹에서는 `isTauri()` 가드로 비활성 |
| `client/src/services/MultiTrackPlayer.ts`, `pages/MusicRoom.tsx` | band 쪽 최신(5ca9b18, e9fa7ad, 428bee8)은 band-tauri를 역포팅한 것이다. 차이는 주석과 함수 순서뿐이다 |
| `server/src/routes/music.ts` (히스토리/인증 부분) | band-tauri가 최신이다: 로그인 필수(d299729), 한글 파일명 디코딩, 중복 파일명 처리. `cleanup-expired` 엔드포인트는 band-tauri가 bf921ed에서 의도적으로 제거해 node-cron으로 대체 |
| `client/src/services/musicApi.ts`, `paymentApi.ts` (`getApiUrl` 'auto') | LAN/자동 호스트 감지(band-tauri 기능) |
| `client/.env.example` ('auto') | 위와 같음 |
| `server/src/services/demucsProvider.ts`, `demucsService.ts` | band-tauri는 `PYTHON_PATH` 환경 변수를 쓴다(4f8f5a4, Windows 수정). band는 `py -3.11`로 하드코딩 |
| `server/Dockerfile` (node:22) | af518de: Supabase WebSocket 크래시 수정. band는 node:20 |
| `server/.gitignore` (`certs`), 루트 `.gitignore` (`.mcp.json`) | band-tauri 쪽 보안 수정(71dcf56) |
| `client/package.json`, 루트 `package.json` | `tauri`, `tauri:dev`, `tauri:build`, `dev:tauri` 스크립트와 `@tauri-apps/cli` |
| `.github/workflows/deploy-server.yml` | band는 3b7ef2f에서 이 워크플로를 지웠다(서버 배포를 band-tauri로 일원화). band-tauri에 유지 |
| `client/vite.config.ts` | band-tauri 쪽 `certs/` 기반 HTTPS, 고정 포트 5173(`tauri.conf.json` devUrl과 일치)을 유지하고 인증서가 없을 때도 설정 로드가 실패하지 않도록 수정. band는 `@vitejs/plugin-basic-ssl`을 쓴다 |
| `client/src/lib/tauri.ts`, `useTauriAudio.ts`, `src-tauri/**`, `docs/overhaul/*`, `docs/webrtc-p2p-connection.md`, `.mcp.json.example` | band-tauri에만 있음 |
| `server/src/index.ts`, `wsService.ts` | `index.ts`는 주석만 다르고 `wsService.ts`는 동일. `wsService.ts`는 수정 금지 대상 |

## 보류함 (이식 안 함)

| 항목 | 이유 |
|---|---|
| `RoomContext.tsx`: 새 연주자 감지 시 "관람자는 offer를 보내지 않음"(9178625), 연주 중일 때만 offer 전송(Glare 방지), `participant-joined` 시 offer 처리 | band-tauri 쪽 같은 위치가 Tauri P2P discovery 분기(`nativeAudioActiveRef`, `p2p-discovery`)와 섞여 있다. RoomContext 재작성 담당자가 판단해야 한다(웹 관람자의 WebRTC 수신 흐름을 바꾸는 변경) |
| `RoomDetail.tsx`: `startLocalMic()` 이후 `setMyInstrument()` 호출 + 실패 시 `setIsPerformer(false)` 롤백(9178625) | 연주 시작 순서가 네이티브 오디오 시작 흐름과 맞물린다. 재작성 시 반영을 권장한다(웹은 연주 불가라 현재 웹 영향은 없음) |
| `musicApi.getStemSeparation()` + `GET /api/music/stem-separation/:id` | band 클라이언트에서도 쓰이지 않는 죽은 API다. band-tauri 히스토리 재구현(bf17442)에서 빠졌다 |
| `POST /api/music/cleanup-expired` | band-tauri가 bf921ed에서 의도적으로 제거했다(node-cron으로 대체) |
| `docs/예외처리반 3차프로젝트 계획보고서.pdf`, `완료보고서.pdf` | 코드/설정 범위 밖의 바이너리(합계 1.8MB) 보고서. 필요하면 별도로 추가 |
| `client/.env`, `.mcp.json` (band에서 git 추적 중) | 비밀값이 들어 있어 가져오지 않음. **band 리포에서 이 파일들이 커밋되어 있으니 키 교체를 권장** |
| `server/tmpclaude-a37f-cwd` | 임시 파일 |
| `.claude/settings.local.json` | 로컬 권한 설정. 차이는 band-tauri에 2줄 더 있는 것뿐 |
| `client/vite.config.ts`의 `@vitejs/plugin-basic-ssl` 방식 | band-tauri의 certs 방식과 고정 포트를 Tauri devUrl이 전제로 한다 |

## 웹 배포 확인

- `client/vercel.json`은 두 리포가 같다(SPA rewrite).
- 루트 `package.json`의 workspaces 구성과 `client`의 `build` 스크립트(`tsc && vite build`)가 같다. 추가 스크립트는 Tauri용뿐이다.
- `vite.config.ts`가 설정 로드 시 `certs/*.pem`을 무조건 읽던 것을 존재할 때만 읽도록 바꿨다(인증서가 없는 빌드 환경 대비. 현재 certs는 git에 추적됨).
- `window.__TAURI__` 접근: `lib/tauri.ts`의 `isTauri()`는 `typeof window` 확인 후 `!== undefined`로 비교하고, `invoke()`는 비Tauri에서 예외를 던진다. 호출부(`AudioSettings`, `AudioSettingsContext`, `RoomContext`, `RoomDetail`)는 모두 `isTauri()`/`isNativeMode`로 가드하거나 try/catch로 감싼다. 웹에서 런타임 에러는 없다.
- band 리포에는 배포 워크플로가 없다(3b7ef2f에서 삭제). Vercel은 대시보드 Git 연동으로 배포하는 것으로 보인다. 통합 후에는 Vercel 프로젝트를 band-tauri 리포(Root Directory `client`)로 다시 연결해야 한다.
- 루트 `package-lock.json`: `server/package.json`에 이미 있던 `node-cron`(bf921ed)이 lock에 빠져 있어 `npm install` 시 갱신됐다.

## 빌드/린트 결과

| 항목 | 작업 전 | 작업 후 |
|---|---|---|
| `client npm run build` | 성공 | 성공 |
| `client npm run lint` | 25 problems (기존 errors/warnings, 실패) | 25 problems, 작업 전과 같은 목록(새 문제 0) |
| `server npm run build` | 성공 | 성공 |

lint는 작업 전부터 `--max-warnings 0`에서 실패한다(`any`, unused 변수, exhaustive-deps 등). 설계 문서 7절의 "lint 통과"는 별도 정리 작업이 필요하다.
