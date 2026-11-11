/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_SIGNALING_URL?: string
  readonly VITE_API_URL?: string
  /** 개발 서버 프록시(vite/bandServerProxy)가 켜졌을 때 로컬 서버 기본 스킴. 플러그인만 정의한다(운영 빌드엔 없음) */
  readonly VITE_BAND_DEV_PROXY?: string
  /** 웹 관람자에게 보여 줄 데스크톱 앱 다운로드 링크 */
  readonly VITE_DESKTOP_DOWNLOAD_URL?: string
  /** 초대 링크에 쓸 웹 주소 (데스크톱 앱은 origin이 tauri:// 라서 필요) */
  readonly VITE_PUBLIC_WEB_URL?: string
  /** 결제 문의 메일 주소. 비우면 결제 화면에서 메일 대신 결제 상태 확인 경로를 안내 */
  readonly VITE_SUPPORT_EMAIL?: string
}
