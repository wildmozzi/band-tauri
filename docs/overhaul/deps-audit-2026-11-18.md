# 의존성 취약점·노후 점검 (2026-11-18)

도구: `npm audit`(workspaces client·server, `--omit=dev` 구분), `cargo audit` 0.23.0, `cargo outdated` 0.19.0. 대상: client(웹·데스크톱 프런트), server, client/src-tauri(앱·band-engine·band-dsp), relay.

## 이 PR에서 고친 것 (semver 범위 안 업그레이드, 잠금 파일만)

| 영역 | 전 → 후 (High/Critical) | 바꾼 것 |
|---|---|---|
| server(운영) | critical 1·high 7 → **high 1(zod, 메이저 필요)** | axios 1.13.2→1.21.1, body-parser 2.2.1→2.4.0(express), multer 2.0.2→2.5.0, path-to-regexp 8.3.0→8.4.2, proxy-addr 2.0.7→2.0.8(**critical**: IPv4-mapped IPv6 trust 우회), qs 6.14.0→6.17.1, ws 8.18.3→8.23.0, form-data·follow-redirects, uuid 13.0.0→13.0.2 |
| client 운영 의존(`--omit=dev`) | critical 1·high 8 → **high 6(전부 OSMD 선택 의존 `gl`의 설치 도구 체인)** | react-router(-dom) 7.10.1→7.18.5(XSS 오픈 리다이렉트 등), tar 6→7.5.23(critical), opensheetmusicdisplay 1.9.3→1.9.9(패치) |
| client 개발 의존 | — | rollup 4.53→4.68, postcss 8.5.6→8.5.33, nanoid, @babel/core, browserslist 등 |
| Rust 앱 | 취약점 7 → **4(메이저·상위 크레이트 필요)** | bytes 1.11.0→1.12.1(RUSTSEC-2026-0007), time 0.3.44→0.3.55(-0009), zip 9.0.0→9.6.1(-0548), tokio 1.48→1.54.1·rand 0.8.5→0.8.6·anyhow·rustix(unsound 경고) |
| relay | 취약점 0 | 변경 없음 |

## 남은 것 — 백로그 후보

| # | 우선 | 대상 | 내용 | 실제 영향 판단 | 필요한 일 |
|---|---|---|---|---|---|
| 1 | ~~중간~~ 고침(chore/zod4) | server `zod` 3.25.76 → 4.6.7 | GHSA-mrhx-c7xc-m3w5(High): 큰 제약 컬렉션 검증 시 메모리 소모. 3.x에는 수정 없음 | JSON 본문 1MB 상한(`express.json`)이 크기를 묶는다. 스키마는 `routes/schemas.ts` 하나 | zod 4 메이저(스키마 API 변경 확인) |
| 2 | 중간 | client 개발 도구 `vitest` 2 / `vite` 5 | vitest·tinypool critical(UI 서버·프로토타입 오염), vite·esbuild 개발 서버 경로 노출 | **개발 서버·시험 실행에만** 있다. 배포 번들에는 없다. 개발 기계를 로컬 밖에 노출하지 않으면 영향 없음 | vite 8 + @vitejs/plugin-react 6 + vitest 5 메이저 묶음(설정·플러그인 호환) |
| 3 | 낮음 | client 개발 `@typescript-eslint` 6 / `eslint` 8 | minimatch·micromatch ReDoS(린트 도구) | 개발 도구, 신뢰하는 소스만 읽음 | typescript-eslint 8 + eslint 9/10(flat config 전환) |
| 4 | 낮음 | client `opensheetmusicdisplay` 1.9.9 → 선택 의존 `gl@9.0.0-rc.10` → node-gyp 12 → tinyglobby → `picomatch` ≤4.0.7 | GHSA-fc5q-39xc-vjpf ReDoS. **수정 버전 미배포**(최신 4.0.7) | 설치 시 네이티브 빌드 도구 체인. 브라우저 번들에는 없다. OSMD 1.9.9가 `gl` rc를 선택 의존으로 지정 | picomatch 수정 배포 뒤 `npm update`. 또는 OSMD 2.x 메이저(데모 악보 화면 `ScoreDisplay`) |
| 5 | 낮음 | server `node-cron` 3 → 내부 `uuid` 8 | GHSA-w5hq-g745-h8pq(moderate): v3/v5/v6에 buf를 줄 때만 | node-cron은 내부적으로 v4만 쓴다 → 해당 없음 | node-cron 4 메이저 |
| 6 | 중간 | Rust `ringbuf` 0.4.8 | RUSTSEC-2026-0293: `Consumer::skip`/`clear`에서 원소 `Drop`이 panic하면 double free | **해당 없음**(2026-11-21 재조사로 근거 정정, 호출 지점 전수는 아래 "ringbuf 0.6 검토"). 우리가 직접 부르는 `skip`/`clear`는 원소가 모두 Drop 글루 없는 타입(`AudioPacket`·`f32`·`RecBlock`)이고 정적 단언·`Copy` 바운드로 묶었다. Drop 있는 원소 링 4종(`Box<PeerMedia>`·`MediaCmd`·`Box<EchoProbe>`·`RecordRt`)은 비어 있지 않은 채 드롭되면 링의 `Drop`이 부르는 `clear`를 거치지만, 그 뒤 링을 다시 방문하지 않아 원소를 한 번씩만 드롭한다. 릴리스는 `panic = "abort"`라 unwind 자체가 없다 | **올리지 않음**. 재검토 조건: ① 0.6.x 후속판이 쌓였을 때 ② Drop 있는 원소에 `skip`/`clear`를 직접 쓰게 될 때(정적 단언·바운드가 깨진다) ③ 릴리스에서 `panic = "abort"`를 뺄 때 |
| 7 | 낮음 | Rust `quick-xml` 0.38 (tauri-utils → plist) | RUSTSEC-2026-0194·0195 DoS | 빌드·코드 생성 때 우리 plist만 파싱 | tauri 쪽 갱신을 기다림 |
| 8 | 정보 | Rust `rkyv` 0.7 (tauri-plugin-log → byte-unit → rust_decimal) | RUSTSEC-2026-0235 | Cargo.lock에만 있고 기능이 꺼져 빌드 그래프(`cargo tree -e normal`)에 없음 | 없음(상위 갱신 때 사라짐) |
| 9 | 정보 | Rust unmaintained/unsound 경고 | audiopus_sys·chrono·pin-utils·proc-macro-error(유지 중단), glib 0.18(tauri GTK, Linux 전용)·tokio(수정됨)·rand 0.8(0.8.6으로 수정) | Windows 대상 빌드에 영향 없음 | audiopus_sys는 Opus 바인딩 교체 검토(엔진) |

## 노후(메이저) 요약

- **client**: react·react-dom 18 → 19, @types/react 18 → 19, vite 5 → 8, vitest 2 → 5, @vitejs/plugin-react 4 → 6, eslint 8 → 10, @typescript-eslint 6 → 8, eslint-plugin-react-hooks 4 → 7, jsdom 25 → 31, typescript 5.9 → 7.0, opensheetmusicdisplay 1.9 → 2.3, @types/node 24 → 26
- **server**: zod 3 → 4, node-cron 3 → 4, uuid 13 → 14, dotenv 17 → 18, typescript 5.9 → 7.0
- **Rust 앱**(직접 의존, 0.x 마이너 = 호환 깨짐):
  - 오디오·암호: cpal 0.15 → 0.18, rubato 0.16 → 5.1, opus 0.3 → 0.4, ringbuf 0.4 → 0.6, chacha20poly1305 0.10 → 0.11, x25519-dalek 2 → 3, hkdf·hmac·sha2 0.12/0.10 → 0.13/0.11
  - 그 밖: rand 0.8 → 0.10, parking_lot 0.12 → 0.13, windows-sys 0.59 → 0.61, if-addrs 0.13 → 0.15, base64 0.22 → 0.23
  - **오디오 경로·암호 포맷에 닿는 것은 엔진 별도 PR**(루프백 수치·패킷 호환 확인)
- **relay**: hmac 0.12 → 0.13, sha2 0.10 → 0.11(앱과 같은 토큰 벡터로 묶어서)

## ringbuf 0.6 검토 (2026-11-21, P3-20b) — 올리지 않음

RT 콜백과 워커 사이의 사전할당 SPSC 링이라, 이득 없이는 바꾸지 않는다는 기준으로 봤다. 올릴 이유 셋(우리 사용 패턴에 해당하는 soundness 수정 / 유지보수·의존성 문제 / 측정 가능한 이득) 가운데 하나도 없었다.

- **버전**: 0.4.8(2025-03, 사용 중) → 0.5.0~0.5.3(2026-05~10) → 0.6.0(2026-10-30, 최신). 0.4.9는 yank됐고 0.4 줄에는 RUSTSEC-2026-0293 수정이 없다.
- **0.4.8 뒤의 수정이 우리에게 닿는가**
  - RUSTSEC-2026-0293(`skip`/`clear` 중 원소 `Drop` panic → double free, 0.5.2): 해당 없음. 권고의 경로는 "`skip`/`clear`가 원소를 드롭하다 panic → 읽기 인덱스가 안 옮겨짐 → 나중에 링이 드롭될 때 같은 원소를 다시 드롭"이다. `client/src-tauri` 전체에서 ringbuf 끝점의 `skip(`·`clear(`·`pop_iter`·`freeze` 호출을 전수 조사한 결과(`pop_iter`·`freeze`는 0건):

    | 호출 지점 | 호출 | 원소 타입 | 가드 |
    |---|---|---|---|
    | `band-dsp/src/media_link.rs` `MediaRx::drain` | `skip` | `AudioPacket` | 타입 옆 `needs_drop` 단언 |
    | `band-engine/src/engine/rt.rs` `OutputRt::mix_source`(SLOT_DRAIN, RT) | `clear` | `f32`(`SourceSample`) | `needs_drop` 단언(링 선언과 같은 별칭) |
    | `band-engine/src/engine/mod.rs` `subscribe_mix`의 탭 스레드 | `clear` | `f32`(`rt::TapSample`) | 같음 |
    | `band-engine/src/engine/measure/loopback.rs` `run` 1곳·`run_inner` 2곳(`ends.capture`) | `clear` | `f32`(`rt::CaptureSample`) | 같음 |
    | `band-engine/src/engine/record/rt_ring.rs` `touch_all`(녹음 링 프리폴트) | `skip` | `f32`·`RecBlock` | `T: Copy + Default` 바운드(`Copy`는 `Drop`을 가질 수 없다) |
    | `band-engine/src/engine/record/tests.rs`(시험) | `skip` | `f32`·`RecBlock` | 시험 코드, 같은 링 |

    여기에 **간접 호출**이 하나 있다: 0.4.8의 `SharedRb`는 `Drop`에서 `self.clear()`를 부른다. 그래서 Drop 있는 원소 링 4종(`Box<PeerMedia>` 반환 큐·`MediaCmd` 명령 큐·`Box<EchoProbe>` 큐 2개·`RecordRt` 핸드오프 2개)도 비어 있지 않은 채 드롭되면 권고가 지목한 함수를 거친다. 다만 이것은 링의 마지막 방문이다 — 원소 `Drop`이 panic해도 그 뒤 링을 다시 드롭하는 단계가 없어(저장소는 `MaybeUninit`이라 원소를 다시 건드리지 않는다) double free가 아니라 남은 원소 누수로 끝난다. 평소에 이 원소들은 `try_pop`으로 나가는데, `try_pop`은 값을 꺼내고 인덱스를 옮긴 뒤에 돌려준다(드롭은 호출자 쪽). 그리고 릴리스는 `panic = "abort"`라 unwind 자체가 없다.
  - 지연 커밋(`Frozen`·`PopIter`)을 `mem::forget`하면 불건전(0.5.3에서 제거): 우리는 `freeze`·`pop_iter`를 쓰지 않는다. `HeapProd`/`HeapCons`(Caching)는 0.4.8에서도 연산마다 바로 커밋한다.
  - 용량 상한·인덱스 overflow 처리(0.5.3): 용량이 `usize::MAX / 2`를 넘을 때만이다. 우리 용량은 작은 상수다.
- **0.6.0 쪽 위험**: 발행 3주째이고, 발행 당일 unsafe 계약을 크게 고쳤다(`Raw` 트레이트 신설, `Frozen` 제거, Caching 끝점 재작성). 인덱스 계산(`traits/utils.rs` `modulus`)에 도달할 수 없기는 하나 `expect` 분기가 새로 들어와 `push_slice`/`pop_slice` 경로에 놓인다.
- **성능**(스크래치 크레이트, 릴리스 프로필과 같은 설정, 0.4.8·0.6.0 교차 4회): `vacant_len`+`push_slice`+`occupied_len`+`pop_slice` 한 묶음은 256샘플에서 0.4.8 26.9~97.4ns / 0.6.0 25.8~93.8ns, 960샘플에서 216~257ns / 112~252ns — 회차 간 흔들림(같은 판이 26→97ns)이 판 간 차이보다 크다. `try_push`+`try_pop`은 12~26ns → 5~16ns로 빨라지지만 콜백·워커 반복(약 1ms)당 몇 번뿐이라 엔진 CPU로는 드러나지 않는다.
- **이행 비용**(나중에 올릴 때): 우리가 쓰는 것(`HeapRb`/`HeapProd`/`HeapCons`, `traits::{Consumer, Observer, Producer, Split}`, `push_slice`·`pop_slice`·`try_push`·`try_pop`·`vacant_len`·`occupied_len`·`as_slices`·`skip`·`clear`)은 0.6에도 같은 이름으로 있다(0.6 소스와 스크래치 빌드로 확인, 워크스페이스 빌드는 하지 않았다).
- **전제를 지키는 장치**: 위 표의 가드(`needs_drop` 정적 단언 4곳 — `f32` 링 셋은 링 선언과 단언이 같은 타입 별칭을 써서 원소 타입을 바꾸면 단언이 따라간다 — 과 `touch_all`의 `Copy` 바운드), 릴리스 프로필 `panic = "abort"` 옆 주석. 새 `skip`/`clear` 호출을 자동으로 잡는 장치는 없다 — 추가할 때 이 표와 단언을 같이 갱신한다. 재검토 조건은 표 6번.
