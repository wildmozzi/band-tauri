# DEMUCS 음원 분리 설정 가이드

이 가이드는 음악재생 방 기능을 위한 DEMUCS API 설정 방법을 설명합니다.

## 목차
1. [Docker를 사용한 설정 (권장)](#docker를-사용한-설정-권장)
2. [Python 직접 사용](#python-직접-사용)
3. [문제 해결](#문제-해결)

---

## Docker를 사용한 설정 (권장)

### 사전 요구사항
- Docker Desktop 설치 필요
- 최소 8GB RAM 권장
- (선택) NVIDIA GPU + CUDA 드라이버 (GPU 가속용)

### 1단계: Docker 설치

#### Windows
1. [Docker Desktop for Windows](https://www.docker.com/products/docker-desktop/) 다운로드
2. 설치 프로그램 실행
3. 설치 후 Docker Desktop 실행
4. WSL 2 업데이트가 필요한 경우 안내에 따라 설치

#### Mac
1. [Docker Desktop for Mac](https://www.docker.com/products/docker-desktop/) 다운로드
2. DMG 파일 실행 및 Applications 폴더로 드래그
3. Docker Desktop 실행

#### Linux (Ubuntu/Debian)
```bash
curl -fsSL https://get.docker.com -o get-docker.sh
sudo sh get-docker.sh
sudo usermod -aG docker $USER
```
재로그인 후:
```bash
docker --version  # 설치 확인
```

### 2단계: DEMUCS API 서버 실행

새 터미널(명령 프롬프트/PowerShell/Terminal)을 열고:

#### 기본 실행 (CPU 사용)
```bash
docker run -p 8000:8000 danielfrg/demucs-api
```

#### 백그라운드 실행 (권장)
```bash
docker run -d -p 8000:8000 --name demucs-server danielfrg/demucs-api
```

#### GPU 사용 (NVIDIA GPU가 있는 경우 - 10배 이상 빠름)
```bash
# Windows/Linux with NVIDIA GPU
docker run --gpus all -p 8000:8000 --name demucs-server danielfrg/demucs-api
```

**참고**: 첫 실행 시 Docker 이미지를 다운로드하므로 시간이 걸립니다 (약 2-3GB).

### 3단계: 서버 확인

브라우저에서 다음 URL 접속:
```
http://localhost:8000/docs
```

Swagger API 문서 페이지가 표시되면 성공입니다!

### 4단계: 프로젝트 설정

프로젝트 루트에 `.env` 파일 생성 또는 수정:

**server/.env**
```env
# DEMUCS API URL
DEMUCS_API_URL=http://localhost:8000

# 기존 설정은 유지
HTTP_PORT=3001
PORT=8080
```

### 5단계: 실제 API 코드 활성화

`server/src/services/demucsService.ts` 파일을 열고:

1. **50-70번째 줄 근처의 실제 API 호출 코드 주석 해제**:
```typescript
// 이 부분의 주석을 제거 (/* ... */ 제거)
const formData = new FormData()
formData.append('audio', fs.createReadStream(mp3FilePath))
formData.append('model', 'htdemucs')
// ... (계속)
```

2. **80-100번째 줄 근처의 데모 코드 주석 처리**:
```typescript
// 이 부분을 주석으로 변경
/*
console.log('[DEMUCS] 데모 모드: 더미 MIDI 파일 생성')
// ... (계속)
*/
```

### 6단계: 서버 재시작

```bash
cd server
npm run dev
```

---

## Python 직접 사용

Docker를 사용할 수 없는 경우 Python으로 직접 실행할 수 있습니다.

### 1단계: Python 설치

- Python 3.8 이상 필요
- [Python 다운로드](https://www.python.org/downloads/)

### 2단계: demucs 패키지 설치

```bash
pip install demucs torch torchaudio
```

GPU 사용 시 (NVIDIA):
```bash
pip install demucs torch torchvision torchaudio --index-url https://download.pytorch.org/whl/cu118
```

### 3단계: 명령줄에서 테스트

```bash
demucs path/to/your/song.mp3
```

출력 파일 위치: `separated/htdemucs/song_name/`

### 4단계: 프로젝트 통합 (선택)

`server/python-demucs-example.py` 스크립트를 사용하여 Node.js에서 호출:

```typescript
import { spawn } from 'child_process'

const python = spawn('python', [
  'python-demucs-example.py',
  inputFilePath,
  outputDirectory,
  'htdemucs'
])

python.stdout.on('data', (data) => {
  console.log(`stdout: ${data}`)
})

python.on('close', (code) => {
  console.log(`Process exited with code ${code}`)
})
```

---

## Docker 컨테이너 관리 명령어

### 실행 중인 컨테이너 확인
```bash
docker ps
```

### 로그 확인
```bash
docker logs demucs-server
```

### 로그 실시간 보기
```bash
docker logs -f demucs-server
```

### 컨테이너 중지
```bash
docker stop demucs-server
```

### 컨테이너 시작
```bash
docker start demucs-server
```

### 컨테이너 재시작
```bash
docker restart demucs-server
```

### 컨테이너 삭제
```bash
docker stop demucs-server
docker rm demucs-server
```

### 이미지 삭제 (완전 제거)
```bash
docker rmi danielfrg/demucs-api
```

---

## 문제 해결

### 1. Docker 명령어가 인식되지 않음
- Docker Desktop이 실행 중인지 확인
- 터미널/명령 프롬프트 재시작
- Docker Desktop 재시작

### 2. 포트 8000이 이미 사용 중
다른 포트 사용:
```bash
docker run -p 8001:8000 --name demucs-server danielfrg/demucs-api
```

`.env` 파일도 수정:
```env
DEMUCS_API_URL=http://localhost:8001
```

### 3. 음원 분리가 너무 느림
- **CPU 모드**: 3-5분 짜리 노래 분리에 5-10분 소요 (정상)
- **GPU 모드**: 30초 ~ 1분 소요
- 해결책: NVIDIA GPU가 있다면 `--gpus all` 옵션 사용

### 4. Docker 이미지 다운로드 실패
- 인터넷 연결 확인
- Docker Desktop 재시작
- 방화벽 설정 확인

### 5. GPU를 인식하지 못함
```bash
# NVIDIA Container Toolkit 설치 (Linux)
distribution=$(. /etc/os-release;echo $ID$VERSION_ID)
curl -s -L https://nvidia.github.io/nvidia-docker/gpgkey | sudo apt-key add -
curl -s -L https://nvidia.github.io/nvidia-docker/$distribution/nvidia-docker.list | sudo tee /etc/apt/sources.list.d/nvidia-docker.list
sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
sudo systemctl restart docker
```

### 6. API 응답이 없음
- 서버 로그 확인: `docker logs demucs-server`
- API 문서 페이지 접속 테스트: http://localhost:8000/docs
- curl로 직접 테스트:
```bash
curl -X POST "http://localhost:8000/api/separate" \
  -F "audio=@test.mp3" \
  -F "model=htdemucs"
```

---

## 성능 비교

| 환경 | 3분 음원 분리 시간 |
|------|-------------------|
| CPU (i7) | 약 8-10분 |
| CPU (M1/M2) | 약 5-7분 |
| NVIDIA RTX 3060 | 약 40초 |
| NVIDIA RTX 4090 | 약 20초 |

---

## 대안 서비스

### 1. Replicate API (클라우드)
- URL: https://replicate.com/facebookresearch/demucs
- 장점: 서버 설정 불필요, GPU 사용
- 단점: 유료 (분당 과금)

### 2. Spleeter (Deezer)
- GitHub: https://github.com/deezer/spleeter
- 장점: 빠름, 가벼움
- 단점: DEMUCS보다 품질 낮음

### 3. Open-Unmix
- URL: https://github.com/sigsep/open-unmix-pytorch
- 장점: 오픈소스, 가벼움
- 단점: 정확도 낮음

---

## 추가 참고 자료

- [DEMUCS GitHub](https://github.com/facebookresearch/demucs)
- [DEMUCS API 문서](https://demucs.danielfrg.com/docs/api/)
- [Docker 공식 문서](https://docs.docker.com/)
- [NVIDIA Container Toolkit](https://github.com/NVIDIA/nvidia-docker)

---

문제가 해결되지 않으면 GitHub Issues에 문의해주세요!
