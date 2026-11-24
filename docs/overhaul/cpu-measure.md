# CPU 전·후 비교 측정 방법 (P1-21b, ADR D6 보강)

- 관련: [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md) D6(동작 불변 증거), 근거 측정 [cpu-quiet-2026-11-17.md](cpu-quiet-2026-11-17.md)(P1-14c), 코어 배치 기록(P1-21 1단계, `sys/placement.rs`).
- 도구: `crates/band-engine/tests/cpu_profile.rs`(`six_peer_full_mesh_cpu`, 6엔진 풀메시·6구성), `scripts/cpu-compare.mjs`(교차 실행·통계, 시험 `scripts/cpu-compare.test.mjs`).

## 왜 둘로 나누나

하이브리드 코어 기계(Intel Core Ultra 225H — P 4·E 8·LP-E 2)에서 고정 없이 재면 같은 실행파일도 실행마다 **두 상태**로 갈린다. 워커·수신 스레드가 느린 코어(LP-E로 추정)에 놓이면 워커 CPU가 1.6~2배, `iter_body` p99가 2배로 뛴다(cpu-quiet 2절). 이 상태 섞임 때문에 표준편차가 평균의 20~35%가 되고, ±5% 회귀를 가를 수 없다.

그래서 회귀를 두 종류로 나눠 따로 잰다.

| 측정 | 무엇을 보나 | 어떻게 | 판정 |
|---|---|---|---|
| **1. 반복당 비용**(판정) | 같은 코어 종류에서 워커 반복이 더 비싸졌나 | **코어 고정**(LP-E를 뺀 P+E), 전·후 교차 10라운드 이상 | 쌍별 차 95% t 구간이 +5% 아래(워커 CPU·`iter_body` p99) |
| **2. 배치 확률**(보조) | 코드 변화가 스레드가 느린 코어에 놓일 확률을 바꿨나(수면 패턴 → 전원 조절·EcoQoS 판정 등) | **고정 없음**, 전·후 교차 20라운드 이상 | 실행파일별 "느린 상태 실행" 비율 차이(Fisher 정확 검정) |

1만 하면 실제 앱(고정 없음)에서만 나는 회귀를 놓친다. 2만 하면 상태 섞임 때문에 반복당 비용 차이를 볼 수 없다. 2는 n이 작으면 검정력이 낮다: 10 대 10에서 5/10 대 8/10은 p ≈ 0.35로 가를 수 없었다. 그래서 2는 "확률이 크게 바뀌었는지"(예: 20% → 70%) 정도만 본다.

## 준비

1. 커밋마다 측정 빌드를 만들고 실행파일을 커밋 이름으로 복사한다. 측정 중에는 빌드하지 않는다.
   ```sh
   cd client/src-tauri
   git switch --detach <커밋>
   cargo test --release -p band-engine --test cpu_profile --no-run   # 찍힌 경로의 cpu_profile-*.exe를 복사
   ```
   이 PR(P1-21b) 전 커밋에는 `BAND_CPU_PIN`·`CSV|` 줄이 없다. 그 커밋은 cpu-quiet 부록 패치를 적용하고, 고정은 `start /affinity FFF`로 한다.
2. 기계를 조용하게 둔다: 다른 작업자·빌드 없이, AC 전원. 실행 사이 쿨다운은 20초다.

## 1. 반복당 비용 — 코어 고정

```sh
node scripts/cpu-compare.mjs run --exe before=cpu_profile-<전>.exe --exe after=cpu_profile-<후>.exe \
  --pin pe --rounds 10 --cooldown 20 --set pinned --out cpu-runs.csv
node scripts/cpu-compare.mjs stats --in cpu-runs.csv --before before --after after --set pinned
```

- `--pin pe`는 `BAND_CPU_PIN=pe`를 넘긴다. 측정 도구가 `GetSystemCpuSetInformation`의 코어 표에서 LP-E를 뺀 마스크로 프로세스를 고정하고(`sys::placement::pin_process`, 225H에서 `0xFFF`), 출력에 `코어 고정: 0x…`를 찍는다. `--pin 0x…`로 마스크를 직접 줄 수도 있다(그룹 0).
- 라운드마다 순서를 뒤집는다(짝수 정순·홀수 역순). 같은 라운드의 전·후 값으로 쌍별 차 % = (후 − 전) / 전 평균 × 100을 낸다.
- 통계: 평균, 95% t 구간, 부트스트랩 95% 구간(10,000회, 시드 고정).
  - `stats`의 기본 지표는 `worker_pct_avg6`(워커 CPU 6엔진 평균)와 `iter_body_p99_us_avg6`(측정 창 12초의 워커 반복 본문 p99)다.
  - `--metric`으로 다른 열을 고를 수 있다(`cb_load_avg_pct`, `cb_p99_us_e0`, `per_node_pct` 등 — `CSVHEAD|` 줄 참고).
- 판정은 cpu-quiet 1절과 같다: 구간이 ±5% 안이면 "안", 밖이면 "밖", 걸치면 "걸침". 주장할 수 있는 것은 **"증가 쪽 +5% 회귀 없음"**(구간 상한 < +5%)이다.
  - 걸침이 실행 1회의 튄 값 때문이면 그 라운드를 빼고 다시 보고 근거를 적는다(cpu-quiet 각주¹).
  - 튄 값은 실행 직전 전체 CPU나 Opus 기준값(`opus_ref_us`, 단일 스레드 인코딩 1회)이 같이 튀었는지로 기계 잡음과 가른다.
- `iter_body` p50은 쓰지 않는다. 히스토그램이 16µs 위에서 옥타브당 8칸이라 칸 경계에서 크게 뛴다. p99(칸 폭 약 6~12%)는 보조 증거로 쓴다.

## 2. 배치 확률 — 고정 없는 반복

```sh
node scripts/cpu-compare.mjs run --exe before=… --exe after=… --pin none --rounds 20 --set unpinned --out cpu-runs.csv
node scripts/cpu-compare.mjs placement --in cpu-runs.csv --set unpinned [--lpe-threshold 10]
```

- 실행 하나의 6구성 중 하나라도 워커가 LP-E에 문턱(기본 10%) 이상 놓였으면 "느린 상태 실행"으로 센다(`worker_lpe_pct`, 측정 창 동안 코어 배치 표본 비율). 구성 수가 늘면 "하나라도" 기준의 기본 비율도 올라가므로, 구성을 바꾼 도구 버전끼리는 횟수를 그대로 비교하지 않는다(#207 리뷰 L4).
- 실행파일별 횟수와 쌍마다 Fisher 정확 검정 양측 p를 낸다.
- 배치 열이 없는 옛 원자료는 `--state-config "Opus 2.5ms" --state-threshold 12`로 워커 CPU 기준 상태를 센다(cpu-quiet 2절과 같다).
- 같은 원자료로 `stats --set unpinned`도 볼 수 있다. 다만 상태가 섞여 있어 판정에는 쓰지 않는다. 상태별로 나눠 비교하는 것은 참고용이다.
- 느린 상태 비율이 뚜렷이 늘었다면(예: p < 0.05, 또는 n이 작아도 0/20 → 10/20 같은 큰 변화) 반복당 비용과 별개인 회귀로 보고 조사한다. 원인 후보는 수면 패턴, 스레드 우선순위·QoS, 새 스레드의 배치다.
- 여러 기계(비하이브리드·노트북 전원 절약)에서 얼마나 갈리는지는 **[사람]** 몫이다.

## 3. 스레드 정책 비교 — 같은 실행파일, 조건만 바꿔 교차 (P1-21 2단계)

엔진이 환경변수 `BAND_THREAD_POLICY` = `off` | `highqos`(EcoQoS 끄기) | `pcores`(P 코어 CPU set — 스레드가 그 집합 안에 머문다) | `both`를 직접 읽는다(제품 빌드와 같은 경로. 유효한 값이면 엔진 설정보다 우선하고, 비우면 설정 기본값 = **HighQoS 켬 · P 코어 끔**(P1-21 3단계부터. 그 전 실행파일은 둘 다 끔). 값은 정확히 그 조합이라 `off`는 HighQoS도 끄고 `pcores`는 HighQoS 없이 P 코어만 건다). `cpu-compare.mjs`의 `--exe 이름=경로@정책`이 그 값을 넘긴다.

```sh
X=cpu_profile-<커밋>.exe
node scripts/cpu-compare.mjs run --exe off=$X@off --exe qos=$X@highqos --exe pcores=$X@pcores --exe both=$X@both \
  --pin none --rounds 10 --cooldown 10 --set policy --out policy-runs.csv
node scripts/cpu-compare.mjs summary --in policy-runs.csv --set policy            # 조건×구성별 평균 ± 표준편차 (중앙값), n
node scripts/cpu-compare.mjs stats --in policy-runs.csv --before off --after both --set policy
```

- 조건이 넷이어도 순서는 같다(짝수 라운드 정순, 홀수 라운드 역순). 코어 고정은 하지 않는다(`--pin none`) — 정책이 바꾸는 것이 배치라서다.
- `CSV|` 줄의 `policy`(옛 `high_qos` 자리 — 넘긴 환경변수 값, 비우면 `default`), `worker_p_pct`·`netrecv_p_pct`(측정 창에서 P 코어에 깨어난 비율), `conceal_max_pct` 열을 쓴다. 실행 출력의 `배치 |` 줄 끝에 엔진이 보고한 적용 결과(`threadPolicy`)가 찍힌다.
- **커밋 전·후 비교(1·2절)에는 정책이 섞이지 않게 한다**(#245 리뷰 L4): `cpu-compare.mjs`는 `@정책`이 없는 `--exe`에 `BAND_THREAD_POLICY=off`를 넘긴다(셸에 남은 값·실행파일마다 다른 기본값을 막는다. 엔진 기본값 그대로 재려면 `@default` — **단 기본값이 P1-21 3단계에서 "둘 다 끔" → "HighQoS 켬"으로 바뀌었으므로, 그 변경을 사이에 둔 두 실행파일을 `@default`로 비교하면 정책 차이가 섞인다**. 제품 기본 조건에서의 전·후를 보려면 양쪽에 `@highqos`를 준다). 옛 `BAND_HIGH_QOS`·`--high-qos`는 없어졌다. `BAND_THREAD_POLICY`를 읽지 않는 옛 실행파일(#245 전)은 늘 정책 없음이다. 단 #245의 첫 커밋(`cc3ff01`)만은 기본값이 켬이고 환경변수 이름이 `qos`·`qos+pcores`였다 — 비교 대상으로 쓰지 않는다.
- **전원을 기록한다.** 측정 도중 AC ↔ 배터리가 바뀌면 그 라운드부터 구간을 나누고, 구간별로 따로 판정한다(2026-11-20 측정은 라운드 0이 배터리·전환에 걸려 빼고 AC에서 2라운드를 더 쟀다).
- xrun처럼 드문 꼬리는 평균이 아니라 **실행별 원값과 "xrun이 난 실행 수"**로 적는다(#245 리뷰 M2).
- 결과: [core-placement-2026-11-20.md](core-placement-2026-11-20.md).

## 4. P 코어 정책 4종 × 환경 2종 — `policy_matrix` (P1-21 3단계 ②)

P 코어 "우선 배치"의 기본값을 정하는 측정이다. 3절의 `cpu-compare.mjs` 대신 **한 명령**으로 전체가 돈다(정책 넷이 모두 HighQoS 켬이고, 경합 부하 프로세스를 도구가 직접 띄우고 내린다).

```sh
cd client/src-tauri
cargo run --release -p band-engine --example policy_matrix -- --dry-run     # 계획과 예상 소요 시간만
cargo run --release -p band-engine --example policy_matrix                  # 본측정 전체: 10라운드 × 환경 2 × 엔진 6·2 × 정책 4 × 구성 6, 창 20초 — 약 6시간 40분
cargo run --release -p band-engine --example policy_matrix -- --preset reduced                # 본측정 축소: 구성 3개(opus2.5·pcm5·pcm5+opus) × 엔진 6 — 약 1시간 48분
cargo run --release -p band-engine --example policy_matrix -- --preset reduced --engines 2    # 축소의 제품에 가까운 짝(엔진 2) — 약 1시간 48분
cargo run --release -p band-engine --example policy_matrix -- --minutes 90  # 시간에 맞춰 라운드 수를 정한다(라운드를 줄이기보다 --preset reduced를 권한다)
```

- 정책: `off`(끔 = `BAND_THREAD_POLICY=highqos`) · `pin`(고정 = `highqos+pin`) · `prefer`(선호 힌트, 2안 = `highqos+prefer`) · `adaptive`(조건부 고정, 3안 = `highqos+adaptive`).
- 환경: `quiet`(조용함) · `contended`(경합). 경합은 **다른 프로세스**(같은 실행파일의 부하 모드)가 P 코어 논리 프로세서 수만큼 바쁜 스레드를 띄운다 — 스레드마다 P 코어 CPU set에 묶이고 엔진 스레드와 같은 MMCSS "Pro Audio"로 등록한다(`--hog-prio mmcss`, 기본. `normal`이면 2단계 5절과 같은 보통 우선순위). `--hog-threads`·`--hog-duty`로 세기를 바꾼다. 부하 프로세스는 부모가 죽어도 실행 하나 길이 + 60초 뒤 스스로 끝난다.
- 엔진 수(`--engines`, 기본 `6,2`): 한 프로세스에 띄우는 엔진 수. 6엔진은 2단계와 같은 풀메시라 엔진 스레드 12개(워커 6 + 수신 6)가 P 코어를 두고 **스스로 다툰다** — 제품(엔진 1개 = 스레드 2개)과 다르다. 2엔진(1:1 루프백, 스레드 4개)이 제품에 가까운 모양이다. 같은 라운드 안에서 함께 잰다(#258 리뷰 M2).
- 순서: 짝수 라운드는 조용함 묶음 → 경합 묶음, 홀수 라운드는 반대(느린 변화 상쇄). 묶음 안의 엔진 수·정책 순서는 시드 고정 무작위(`--seed`, 기본 시작 시각 — 결과에 남는다). 경합 실행 바로 뒤의 조용함 실행 앞에서는 `--recover-s`(기본 60초)를 쉬고, 그 밖의 실행 사이는 `--cooldown-s`(기본 5초). 실행마다 직전 실행의 환경·엔진 수·정책과 쉰 시간을 `runs.jsonl`에 남겨 이월을 사후에 가를 수 있다(#258 리뷰 M3). 실행 하나 = 새 프로세스에서 구성마다 (예열 + 측정 창).
- 먼저 **프로브**가 선호 힌트의 강제성을 본다: 보통 우선순위 바쁜 스레드를 P 코어 수의 2배만큼 띄워 `off`·`prefer`·`pin`을 걸고, P에 머문 비율과 프로세스가 쓴 코어 수를 잰다(고정이면 P 100%·코어 ≈ P 수, soft면 E로 넘쳐 코어 ≈ 스레드 수).
- 결과: `--out` 폴더(기본 `policy-matrix-<UTC 시각>`)의 `results.md`(묶음(환경 · 엔진 수)별 표: 워커 CPU, `iter_body` p50/p99, xrun, 최대 은닉, 워커·수신 P 비율, 워커 LP-E 비율, 조건부 고정의 풀기·다시 묶기 횟수, 끔 대비 쌍별 차, xrun 원값, 부하가 실제로 쓴 코어 수) · `results.json`(요약 + 실행별 원값) · `runs.jsonl`(실행이 끝날 때마다 한 줄 — 중간에 멈춰도 남는다).
- 측정 창 기본값이 20초인 이유: 조건부 고정은 1초 창으로 판단하고 푼 뒤 최소 10초를 기다린다 — 2단계의 12초 창으로는 풀린 뒤의 동작이 창에 들어오지 않는다. 그래도 구성마다 엔진을 새로 시작하므로 "오래 버틴 뒤의 동작"(기다림이 늘어나는 이력)은 이 도구로 보지 못한다(판단 로직 단위 시험과 Soak의 몫).
- **주의**: 경합 환경은 실시간 우선순위 스레드로 P 코어를 채운다 — 도는 동안 같은 PC의 다른 일이 느려지고, 다른 작업이 돌면 수치가 오염된다. **조용한 시간에, AC 전원으로** 돌린다(전원 상태는 라운드마다 `results.md` 머리에 기록된다 — 도중에 바뀌었으면 그 라운드부터 따로 본다).
- 표본 통계(평균 ± 표준편차, 중앙값)와 **끔 대비 쌍별 차**(라운드마다 정책 − 끔의 중앙값, 끔보다 나은/나쁜 라운드 수 — `iter_body` p99·워커 CPU·xrun)를 낸다(#258 리뷰 L1). 10라운드에서 9/10 이상이 한쪽이면 부호 검정 p < 0.05다. 구간이 필요하면 `results.json`의 `runs[].rows`로 계산한다.
- 시간이 모자랄 때(#258 리뷰 L2): 라운드를 줄이지 말고 구성을 줄인다(`--preset reduced`). 6라운드면 6/6 일치여도 p ≈ 0.031이고 한 라운드만 어긋나도 유의하지 않다.

## 기록

PR에는 다음을 적는다.
- 1의 표(구성별 전·후 평균(sd), 쌍별 차 평균, 95% t, 부트스트랩, 판정)
- 2의 횟수·p
- 기계·전원·실행 직전 부하
- 원자료 CSV(필요하면 `docs/overhaul/`에 커밋)

통계 정의는 `scripts/cpu-compare.test.mjs`가 cpu-quiet-2026-11-17 원자료로 그 문서의 표(A Opus 2.5ms −1.25% [−4.86, +2.36], m1 높은 상태 5·7·8·7회)를 재현하는지 확인한다.
