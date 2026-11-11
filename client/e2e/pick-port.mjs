// E2E 미리보기 포트 고르기 (P3-0d). port.ts의 findFreePortSync가 별도 node 프로세스로 동기 실행한다
// (Playwright 설정은 동기로 평가되므로). 사용법:
//   node pick-port.mjs <lockDir> <min> <max> <ownerPid> [start]
// 표준출력: 고른 포트, 표준에러: 실패 이유.
//
// 왜 포트 0이 아닌가: "포트 0으로 바인딩해 OS가 준 빈 포트를 확인 → 닫기 → vite preview가 그 포트에 바인딩"은
// 닫는 순간부터 그 포트가 다시 OS의 임시 포트 풀로 돌아가, 같은 PC에서 동시에 고르는 다른 실행(다른 워크트리·
// CI 프로세스)에게도 그대로 다시 나눠줄 수 있다. 호스티드 ubuntu에서 6개 중 2개가 같은 포트를 받았다(#61 CI).
//
// 대신
// 1. OS 임시 포트 범위(리눅스 32768~60999, 윈도우 49152~65535) 밖의 고정 범위에서 고른다. OS가 포트 0 바인딩이나
//    나가는 연결에 이 범위를 나눠주지 않으니, 확인 → 바인딩 틈에 끼어드는 것은 포트를 직접 지정한 프로세스뿐이다.
// 2. 고르기 전에 포트별 잠금 파일(<lockDir>/<port>.lock)을 배타 생성(O_EXCL)한다. 동시에 고르는 e2e 실행끼리는
//    잠금 파일 생성이 원자적이라 같은 포트를 절대 함께 갖지 못한다. 잠금은 소유자(Playwright 메인 프로세스, ownerPid)가
//    끝날 때 지우고(port.ts), 소유자가 죽어 남은 잠금은 다음 실행이 만나면 치운다.
// 3. 잠근 뒤 127.0.0.1에 실제로 배타 바인딩해 본다(다른 프로그램이 쓰는 포트·윈도우 예약 범위는 건너뜀).
import { closeSync, mkdirSync, openSync, readFileSync, statSync, unlinkSync, writeSync } from 'node:fs'
import { createServer } from 'node:net'
import { join } from 'node:path'

const [lockDir, minArg, maxArg, ownerArg, startArg] = process.argv.slice(2)
const min = Number(minArg)
const max = Number(maxArg) // 포함 안 함
const owner = Number(ownerArg)
if (!lockDir || !Number.isInteger(min) || !Number.isInteger(max) || min < 1 || max > 65536 || min >= max || !Number.isInteger(owner)) {
  console.error(`사용법: pick-port.mjs <lockDir> <min> <max> <ownerPid> [start] (받은 값 ${JSON.stringify(process.argv.slice(2))})`)
  process.exit(2)
}
const size = max - min
const start = startArg === undefined ? Math.floor(Math.random() * size) : Number(startArg) % size
/** 한 번에 살펴볼 후보 수(각 후보는 잠금 + 바인딩 확인). 범위가 거의 차 있는 비정상 상태에서 오래 돌지 않게 */
const MAX_TRIES = Math.min(size, 200)
/** 소유자가 살아 있어도 이보다 오래된 잠금은 남은 것으로 본다(PID 재사용 대비) */
const STALE_MS = 24 * 3600 * 1000

function alive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false
  try {
    process.kill(pid, 0)
    return true
  } catch (e) {
    return e.code === 'EPERM' // 있지만 신호 권한이 없음 = 살아 있음
  }
}

/** 잠금 파일 배타 생성. 이미 있으면 소유자가 죽었거나 너무 오래된 것만 치우고 이번 후보는 건너뛴다(false). */
function tryLock(path) {
  try {
    const fd = openSync(path, 'wx')
    writeSync(fd, `${owner} ${Date.now()}\n`)
    closeSync(fd)
    return true
  } catch (e) {
    if (e.code !== 'EEXIST') throw e
  }
  try {
    const pid = Number(readFileSync(path, 'utf8').split(' ')[0])
    if (!alive(pid) || Date.now() - statSync(path).mtimeMs > STALE_MS) unlinkSync(path)
  } catch {
    // 그 사이 소유자가 지웠다 — 다음 실행이 쓰면 된다
  }
  return false
}

function canBind(port) {
  return new Promise(resolve => {
    const s = createServer()
    s.once('error', () => resolve(false))
    s.listen({ port, host: '127.0.0.1', exclusive: true }, () => s.close(() => resolve(true)))
  })
}

mkdirSync(lockDir, { recursive: true })
for (let i = 0; i < MAX_TRIES; i++) {
  const port = min + ((start + i) % size)
  const lock = join(lockDir, `${port}.lock`)
  if (!tryLock(lock)) continue
  if (await canBind(port)) {
    process.stdout.write(String(port))
    process.exit(0)
  }
  unlinkSync(lock)
}
console.error(`${min}~${max - 1}에서 빈 포트를 찾지 못했습니다 (${MAX_TRIES}개 확인, 잠금 ${lockDir})`)
process.exit(1)
