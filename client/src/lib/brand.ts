/** 서비스 이름. 웹 문구·탭 제목은 모두 이 값을 쓴다 (데스크톱 앱 productName은 src-tauri/tauri.conf.json) */
export const SERVICE_NAME = 'IndigoMuse'

/** 웹 관람자에게 보여 줄 데스크톱 앱 다운로드 링크 (비어 있으면 '준비 중'으로 비활성) */
export const DESKTOP_DOWNLOAD_URL: string | undefined = import.meta.env.VITE_DESKTOP_DOWNLOAD_URL?.trim() || undefined
