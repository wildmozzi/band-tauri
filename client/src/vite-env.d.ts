/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_SIGNALING_URL?: string
  /** 웹 관람자에게 보여 줄 데스크톱 앱 다운로드 링크 */
  readonly VITE_DESKTOP_DOWNLOAD_URL?: string
}
