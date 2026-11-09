# BandSpace 결제 기능 설정 가이드

## 개요
BandSpace는 토스페이먼츠를 통한 구독 결제 시스템을 제공합니다. 이 가이드는 결제 기능을 설정하고 테스트하는 방법을 설명합니다.

## 기능 구성

### 플랜 구조
- **무료 플랜**: 기본 합주 기능 (최대 4명, 광고 표시)
- **Standard 플랜**: 월 2,900원 (최대 6명, 클라우드 저장, Mix Lab 기본)
- **Pro 플랜**: 월 6,900원 (최대 8명, 자동 믹싱, 팀 관리, 무제한 저장)

### 결제 방법
- 신용카드
- 계좌이체
- 카카오페이
- 네이버페이

## 설정 방법

### 1. 토스페이먼츠 계정 설정

1. [토스페이먼츠 개발자센터](https://developers.tosspayments.com/)에서 계정 생성
2. 테스트용 API 키 발급:
   - 클라이언트 키: `test_ck_D5GePWvyJnrK0W0k6q8gLzN97Eoq`
   - 시크릿 키: `test_sk_zXLkKEypNArWmo50nX3lmeaxYG5R`

### 2. 환경변수 설정

#### 클라이언트 (.env)
```bash
VITE_API_URL=http://localhost:3001
VITE_TOSS_PAYMENTS_CLIENT_KEY=test_ck_D5GePWvyJnrK0W0k6q8gLzN97Eoq
```

#### 서버 (.env)
```bash
PORT=8080
HTTP_PORT=3001
TOSS_PAYMENTS_SECRET_KEY=test_sk_zXLkKEypNArWmo50nX3lmeaxYG5R
NODE_ENV=development
```

### 3. 패키지 설치

#### 서버
```bash
cd server
npm install express cors dotenv axios @types/express @types/cors
```

#### 클라이언트
```bash
cd client
npm install @tosspayments/payment-sdk
```

## 서버 실행

### 1. 서버 시작
```bash
cd server
npm run dev
```
- WebSocket 서버: `ws://localhost:8080`
- HTTP API 서버: `http://localhost:3001`

### 2. 클라이언트 시작
```bash
cd client
npm run dev
```
- 클라이언트: `http://localhost:5173`

## API 엔드포인트

### 결제 관련 API
- `POST /api/payment/confirm` - 결제 승인
- `POST /api/payment/cancel` - 결제 취소
- `GET /api/payment/subscription/:userId` - 구독 정보 조회
- `POST /api/payment/change-plan` - 플랜 변경
- `GET /api/payment/plans` - 플랜 정보 조회

## 테스트 방법

### 1. 테스트 카드 정보
토스페이먼츠 테스트 환경에서 사용할 수 있는 카드 정보:

- **카드번호**: 4300-0000-0000-0000
- **유효기간**: 아무 미래 날짜
- **CVC**: 아무 3자리 숫자
- **비밀번호**: 아무 2자리 숫자

### 2. 결제 플로우 테스트

1. 회원가입/로그인
2. 요금제 페이지 접속 (`/pricing`)
3. 플랜 선택 후 "시작하기" 클릭
4. 결제 정보 입력
5. 결제 승인 확인
6. 플랜 업그레이드 확인

### 3. 기능 테스트

#### Standard 플랜 기능
- 합주실 최대 6명 참여
- 비공개 방 생성
- 클라우드 저장 (30일)
- Mix Lab 기본 기능
- 광고 제거

#### Pro 플랜 기능
- 합주실 최대 8명 참여
- 자동 믹싱 기능
- 클라우드 저장 무제한
- Mix Lab 고급 기능
- 팀 관리 기능
- 세션 히스토리

## 데이터 구조

### 구독 정보
```typescript
interface Subscription {
  id: string
  userId: string
  planType: 'free' | 'standard' | 'pro'
  status: 'active' | 'cancelled' | 'expired'
  startDate: Date
  endDate: Date
  paymentKey?: string
  amount: number
}
```

### 사용자 정보
```typescript
interface User {
  id: string
  email: string
  nickname: string
  planType: 'free' | 'standard' | 'pro'
  subscriptionId?: string
}
```

## 보안 고려사항

1. **API 키 보안**: 프로덕션에서는 환경변수로 관리
2. **결제 검증**: 서버에서 결제 금액과 플랜 정보 검증
3. **사용자 인증**: 결제 요청 시 사용자 인증 확인
4. **HTTPS 사용**: 프로덕션에서는 HTTPS 필수

## 프로덕션 배포

### 1. 토스페이먼츠 실제 키 발급
- 사업자 등록 후 실제 API 키 발급
- 테스트 키를 실제 키로 교체

### 2. 환경변수 설정
```bash
# 프로덕션 환경변수
TOSS_PAYMENTS_SECRET_KEY=live_sk_실제키
VITE_TOSS_PAYMENTS_CLIENT_KEY=live_ck_실제키
NODE_ENV=production
```

### 3. 데이터베이스 연동
현재는 메모리 저장소를 사용하지만, 프로덕션에서는 PostgreSQL, MongoDB 등 실제 데이터베이스 연동 필요

## 문제 해결

### 자주 발생하는 오류

1. **CORS 오류**: 서버에서 CORS 설정 확인
2. **결제 실패**: 테스트 카드 정보 확인
3. **API 연결 실패**: 서버 실행 상태 및 포트 확인

### 로그 확인
- 서버 콘솔에서 결제 관련 로그 확인
- 브라우저 개발자 도구에서 네트워크 요청 확인

## 추가 기능 개발

### 예정된 기능
- 자동 결제 갱신
- 결제 내역 조회
- 환불 처리
- 쿠폰/할인 시스템
- 팀 단위 결제

### 확장 가능한 결제 방법
- 페이팔
- 애플페이
- 구글페이
- 암호화폐 결제

## 지원

결제 기능 관련 문의:
- 이메일: support@bandspace.com
- 토스페이먼츠 개발자 문서: https://docs.tosspayments.com/