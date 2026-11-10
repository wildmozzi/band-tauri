# 데스크톱 릴리스 · 자동 업데이트 (P2-1)

Windows 우선. 설치본은 NSIS(`Band_<버전>_x64-setup.exe`), 자동 업데이트는 `tauri-plugin-updater` v2.

## 구성 요약

| 항목 | 위치 |
|---|---|
| 앱 버전(단일 출처) | `client/src-tauri/tauri.conf.json` `version` — `scripts/bump-version.mjs`로 `Cargo.toml`/`Cargo.lock`/`client/package.json`/`package-lock.json` 동기화 |
| 업데이터 엔드포인트 | `https://github.com/exception-busters/band-tauri/releases/latest/download/latest.json` (`tauri.conf.json` `plugins.updater.endpoints`) |
| 업데이터 공개키 | `tauri.conf.json` `plugins.updater.pubkey` (minisign) |
| 업데이터 개인키 | **리포 밖**: 생성자 PC `%USERPROFILE%\.tauri\band-updater.key` (+ 비밀번호 `band-updater.key.password`) → GitHub Secrets |
| 릴리스 워크플로 | `.github/workflows/release.yml` (`v*` 태그 push → draft 릴리스) |
| 앱 동작 | `client/src/native/updater.ts`, `client/src/components/UpdateNotice.tsx` |

### 앱 동작
- 앱 시작 시 조용히 확인(오프라인·실패면 아무것도 띄우지 않음). 새 버전이 있으면 하단 배너 "지금 업데이트 / 나중에".
- 설정 → 오디오 설정 화면 아래 **앱 업데이트** 섹션의 "업데이트 확인" 버튼으로 수동 확인.
- **연주 중(엔진 동작 중, `engine_stats().running`)에는 설치하지 않고 알림만** 한다. 연주를 마친 뒤 "다시 시도".
- 설치는 NSIS `passive` 모드(진행 표시만, 질문 없음). 설치 프로그램이 앱을 종료하고 설치 후 다시 실행한다.
- 다운로드한 설치본은 공개키로 서명을 검증한다. 서명이 맞지 않으면 설치하지 않는다.

## 1. 최초 1회: 시크릿 등록 (코디네이터/관리자)

GitHub → Settings → Secrets and variables → Actions.

**Secrets**

| 이름 | 값 |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | `%USERPROFILE%\.tauri\band-updater.key` 파일 **내용 전체** |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `%USERPROFILE%\.tauri\band-updater.key.password` 파일 내용 |
| `VITE_SUPABASE_ANON_KEY` | Supabase anon key (variables에 둬도 됨 — 공개 키) |
| `WINDOWS_CERTIFICATE` | (P2-2, 인증서 확보 후) `.pfx`를 base64 인코딩한 문자열 |
| `WINDOWS_CERTIFICATE_PASSWORD` | (P2-2) `.pfx` 비밀번호 |

**Variables** (secrets에 같은 이름이 있으면 variables가 우선)
`VITE_SIGNALING_URL`, `VITE_API_URL`, `VITE_SUPABASE_URL`, `VITE_TOSS_PAYMENTS_CLIENT_KEY`, (선택) `WINDOWS_TIMESTAMP_URL`(기본 `http://timestamp.digicert.com`).

gh CLI로 등록하는 예 (PowerShell, 키 생성 PC에서):

```powershell
gh secret set TAURI_SIGNING_PRIVATE_KEY --body (Get-Content -Raw "$env:USERPROFILE\.tauri\band-updater.key")
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body (Get-Content -Raw "$env:USERPROFILE\.tauri\band-updater.key.password")
gh variable set VITE_SIGNALING_URL --body "wss://<서버>"
# ... 나머지 VITE_* 동일
```

등록 후 개인키·비밀번호 파일은 **안전한 곳(비밀번호 관리자 등)에 백업**한다. 잃어버리면 기존 설치본은 더 이상 업데이트를 받을 수 없다(새 키로 바꾸면 사용자가 설치본을 수동으로 다시 받아야 함).

### 키를 새로 만들어야 할 때
```powershell
cd client
npx tauri signer generate --ci -p "<비밀번호>" -w "$env:USERPROFILE\.tauri\band-updater.key"
```
출력된 공개키(`band-updater.key.pub` 내용)를 `tauri.conf.json` `plugins.updater.pubkey`에 넣고 커밋한다. **개인키(`band-updater.key`)는 절대 커밋하지 않는다.**

## 2. 릴리스 절차

```powershell
node scripts/bump-version.mjs 0.2.0      # 또는 patch | minor | major
git commit -am "chore: v0.2.0"
# PR → develop → main 병합 (사람 승인) 후 main에서:
git tag v0.2.0
git push origin v0.2.0
```

1. 워크플로가 태그와 앱 버전이 같은지 먼저 확인한다(`bump-version.mjs --check`). 다르면 실패.
2. windows-latest에서 NSIS 설치본 + `.sig` + `latest.json`을 만들어 **draft 릴리스**에 올린다.
3. 사람이 draft를 내려받아 설치·실행 확인 후 **Publish**. Publish 전에는 `releases/latest`가 이전 버전을 가리키므로 기존 사용자에게 업데이트가 뜨지 않는다.
4. prerelease로 Publish 하면 `latest`에 잡히지 않는다 — 베타 채널이 필요하면 엔드포인트를 따로 둔다(미구현).

### 로컬 빌드
```powershell
cd client
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -Raw "$env:USERPROFILE\.tauri\band-updater.key"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = Get-Content -Raw "$env:USERPROFILE\.tauri\band-updater.key.password"
npm run tauri build
```
`createUpdaterArtifacts: true`라서 서명 키 env가 없으면 빌드 마지막 단계에서 실패한다. 업데이트 산출물 없이 설치본만 필요하면 `npm run tauri build -- --config '{\"bundle\":{\"createUpdaterArtifacts\":false}}'`.

산출물: `client/src-tauri/target/release/bundle/nsis/Band_<버전>_x64-setup.exe` (+ `.sig`).

로컬 측정 (2026-11-10, v0.1.0, WASAPI 빌드, Authenticode 서명 없음):
- `Band_0.1.0_x64-setup.exe`: **3,011,948 B (≈2.9 MB)** + `.sig` 412 B
- 설치되는 `band.exe`: 7,393,280 B (≈7.1 MB, LTO·`opt-level="s"`·strip)
- 업데이터 서명(`.sig`) 생성 확인. 설치·실행·업데이트 적용은 [사람] 검증 대상.

## 3. 코드 서명(Authenticode)과 SmartScreen — P2-2 [사람]

현재 **인증서가 없어 설치본이 서명되지 않는다.** 워크플로는 `WINDOWS_CERTIFICATE` 시크릿이 있을 때만 인증서를 가져와 `certificateThumbprint`로 서명하고, 없으면 경고만 남기고 서명 없이 빌드한다.

서명 없는 설치본에서 사용자가 보는 것:
- **Microsoft Defender SmartScreen**: "Windows의 PC 보호" 창. **추가 정보 → 실행**을 눌러야 설치된다.
- **스마트 앱 컨트롤(Windows 11, 켜져 있는 경우)**: 서명 없는/평판 없는 앱을 **차단하고 우회 버튼이 없다.** 사용자는 설정 → 개인 정보 및 보안 → Windows 보안 → 앱 및 브라우저 컨트롤 → 스마트 앱 컨트롤을 끄는 수밖에 없다(한 번 끄면 Windows 재설치 전까지 다시 켤 수 없음). 사용자에게 끄라고 안내하지 말고, 서명된 설치본을 배포하는 것이 해결책이다.
- 브라우저 다운로드 경고("일반적으로 다운로드되지 않는 파일").
- 자동 업데이트 자체는 minisign 서명으로 검증하므로 Authenticode와 무관하게 동작한다. 단, 업데이트로 받은 설치본도 스마트 앱 컨트롤에는 같은 규칙이 적용된다.

인증서 확보 후 할 일 (P2-2):
1. OV/EV 코드 서명 인증서 또는 Azure Trusted Signing 계정 확보(유료 — 사람 결정).
2. `.pfx`를 base64로: `[Convert]::ToBase64String([IO.File]::ReadAllBytes("cert.pfx")) | Set-Clipboard` → `WINDOWS_CERTIFICATE` 시크릿, 비밀번호는 `WINDOWS_CERTIFICATE_PASSWORD`.
3. 태그 릴리스 후 설치본 속성 → 디지털 서명 탭에서 서명·타임스탬프 확인.
4. EV가 아니면 SmartScreen 평판은 다운로드가 쌓여야 생긴다. 하드웨어 토큰 기반 EV/Azure Trusted Signing은 `signCommand` 방식으로 워크플로를 바꿔야 한다(현재 단계는 `.pfx` 전제).
