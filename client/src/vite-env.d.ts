/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_SIGNALING_URL?: string
  /** 웹 관람자에게 보여 줄 데스크톱 앱 다운로드 링크 */
  readonly VITE_DESKTOP_DOWNLOAD_URL?: string
  /** 초대 링크에 쓸 웹 주소 (데스크톱 앱은 origin이 tauri:// 라서 필요) */
  readonly VITE_PUBLIC_WEB_URL?: string
  /** 결제 문의 메일 주소. 비우면 결제 화면에서 메일 대신 결제 상태 확인 경로를 안내 */
  readonly VITE_SUPPORT_EMAIL?: string
}
