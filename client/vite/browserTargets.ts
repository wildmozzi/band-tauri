/**
 * 웹(관람)·데스크톱 빌드의 브라우저 지원 하한 (P3-20b 리뷰 M-1).
 *
 * vite 5 기본값('modules')과 같은 값을 명시한다. vite 8 기본값(baseline-widely-available = chrome111·safari16.4 …)을 그대로 쓰면
 * Lightning CSS가 `@media (max-width: …)`를 범위 문법 `(width<=…)`으로 바꿔 iOS/Safari 16.4 미만에서 모든 미디어 쿼리가
 * 무시된다(모바일 관람 화면이 데스크톱 레이아웃으로 나옴). 하한을 올릴지는 사용자 결정 — 올리면 첫 화면 JS gzip 약 −2.9 kB.
 * 빌드된 CSS에 범위 문법이 없는지는 e2e/bundle-budget.e2e.ts가 검사한다.
 */
export const BROWSER_TARGET = ['es2020', 'edge88', 'firefox78', 'chrome87', 'safari14']
