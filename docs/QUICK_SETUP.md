# 빠른 설정 가이드 - Python 사용

Docker 이미지 문제로 Python을 직접 사용하는 것이 더 간단합니다.

## 1단계: Python 설치 확인

PowerShell을 열고:

```powershell
python --version
```

**Python이 없다면:**
1. https://www.python.org/downloads/ 접속
2. "Download Python" 클릭
3. 설치 시 **반드시 "Add Python to PATH" 체크**
4. 설치 후 PowerShell 재시작

## 2단계: demucs 설치

```powershell
pip install demucs torch torchaudio
```

설치 시간: 약 2-5분

## 3단계: 테스트

```powershell
demucs --help
```

도움말이 나오면 성공!

## 4단계: 프로젝트 코드 수정

### 파일 1: `server/src/services/demucsService.ts`

기존 파일을 `demucsService-python.ts`의 내용으로 교체:

1. `demucsService-python.ts` 파일 열기
2. 전체 내용 복사
3. `demucsService.ts` 파일에 붙여넣기 (기존 내용 덮어쓰기)

### 파일 2: `server/src/routes/music.ts`

상단에 import 확인:
```typescript
import { separateAudioStems } from '../services/demucsService'
```

변경 없음! 그대로 사용하면 됩니다.

## 5단계: 서버 재시작

```powershell
cd server
npm run dev
```

## 6단계: 테스트

1. 브라우저에서 http://localhost:5173 접속
2. "노래방(데모)" → "음악재생 방" 탭
3. MP3 파일 업로드
4. 분리 완료까지 대기 (3분 곡 기준 5-10분)

---

## 문제 해결

### "python을 찾을 수 없습니다"
- Python 설치 확인
- PowerShell 재시작
- 시스템 재부팅

### "pip를 찾을 수 없습니다"
```powershell
python -m pip install demucs torch torchaudio
```

### 설치가 너무 느림
정상입니다. torch 패키지가 큽니다 (약 2GB).

### GPU 사용하려면
NVIDIA GPU가 있는 경우:
```powershell
pip install demucs torch torchaudio --index-url https://download.pytorch.org/whl/cu118
```

---

## 성능 예상

| CPU | 3분 음원 분리 시간 |
|-----|-------------------|
| i5/i7 (일반) | 8-12분 |
| i7/i9 (고성능) | 5-8분 |
| M1/M2 Mac | 4-6분 |
| NVIDIA GPU | 30초-1분 |

---

이 방법이 가장 간단하고 확실합니다!
