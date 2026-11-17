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

- 실행 하나의 6구성 중 하나라도 워커가 LP-E에 문턱(기본 10%) 이상 놓였으면 "느린 상태 실행"으로 센다(`worker_lpe_pct`, 측정 창 동안 코어 배치 표본 비율).
- 실행파일별 횟수와 쌍마다 Fisher 정확 검정 양측 p를 낸다.
- 배치 열이 없는 옛 원자료는 `--state-config "Opus 2.5ms" --state-threshold 12`로 워커 CPU 기준 상태를 센다(cpu-quiet 2절과 같다).
- 같은 원자료로 `stats --set unpinned`도 볼 수 있다. 다만 상태가 섞여 있어 판정에는 쓰지 않는다. 상태별로 나눠 비교하는 것은 참고용이다.
- 느린 상태 비율이 뚜렷이 늘었다면(예: p < 0.05, 또는 n이 작아도 0/20 → 10/20 같은 큰 변화) 반복당 비용과 별개인 회귀로 보고 조사한다. 원인 후보는 수면 패턴, 스레드 우선순위·QoS, 새 스레드의 배치다.
- 여러 기계(비하이브리드·노트북 전원 절약)에서 얼마나 갈리는지는 **[사람]** 몫이다.

## 기록

PR에는 다음을 적는다.
- 1의 표(구성별 전·후 평균(sd), 쌍별 차 평균, 95% t, 부트스트랩, 판정)
- 2의 횟수·p
- 기계·전원·실행 직전 부하
- 원자료 CSV(필요하면 `docs/overhaul/`에 커밋)

통계 정의는 `scripts/cpu-compare.test.mjs`가 cpu-quiet-2026-11-17 원자료로 그 문서의 표(A Opus 2.5ms −1.25% [−4.86, +2.36], m1 높은 상태 5·7·8·7회)를 재현하는지 확인한다.
