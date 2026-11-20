/**
 * markup-styles.tool.ts가 비교하는 마크업 조각. 컴포넌트 JSX의 className 구조를 옮겨 적었다(데스크톱 전용 화면 위주).
 * data-probe가 붙은 요소의 계산 스타일을 비교한다. 상위 구조는 RoomDetail.tsx의 연주 화면(live-room > live-body)을 따른다.
 */
export interface MarkupFragment {
  name: string
  html: string
  /** 이 선택자에 마우스를 올린 상태로 잰다 */
  hover?: string
  /** 메인 CSS 뒤에 붙일 지연 청크 CSS를 찾는 문자열 */
  chunks?: readonly string[]
}

export const STYLE_PROPS = [
  'display', 'position', 'width', 'height', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'padding-top', 'padding-right', 'padding-bottom', 'padding-left', 'border-top-width', 'border-top-style', 'border-top-color',
  'border-top-left-radius', 'color', 'background-color', 'background-image', 'opacity', 'transform', 'box-shadow',
  'font-size', 'font-weight', 'line-height', 'white-space', 'flex-grow', 'flex-shrink', 'flex-basis', 'gap', 'text-align', 'cursor', 'z-index',
] as const

const room = (inner: string) =>
  `<div class="app"><main class="app-content"><div class="live-room"><div class="live-body">${inner}</div></div></main></div>`

export const MARKUP_FRAGMENTS: readonly MarkupFragment[] = [
  {
    name: '참여 토글 — 연주 시작 버튼',
    html: room(`<aside class="performers-panel"><div class="participation-toggle"><button class="toggle-btn performer" data-probe="performer">연주</button><button class="toggle-btn viewer" data-probe="viewer">관람</button></div></aside>`),
  },
  {
    name: '참여 토글 — 연주 시작 버튼 hover',
    html: room(`<aside class="performers-panel"><div class="participation-toggle"><button class="toggle-btn performer" data-probe="performer">연주</button></div></aside>`),
    hover: '.toggle-btn.performer',
  },
  {
    name: '참여 토글 — 비활성 연주 버튼 hover',
    html: room(`<aside class="performers-panel"><div class="participation-toggle"><button class="toggle-btn performer" disabled data-probe="performer">연주</button></div></aside>`),
    hover: '.toggle-btn.performer',
  },
  {
    name: '믹서 마스터 레벨',
    html: room(`<section class="mixer-panel"><div class="mixer-master"><div class="master-header"><button class="master-mute-btn" data-probe="mute">🔊</button></div>
      <div class="master-control-row"><label data-probe="mlabel">볼륨</label><input type="range" data-probe="mrange"><span data-probe="mspan">100</span></div>
      <div class="level-meter master-level" data-probe="meter"><div class="level-bar" data-probe="bar"><div class="level-fill" data-probe="fill"></div></div></div></div></section>`),
  },
  {
    name: '믹서 채널 레벨·조절 줄',
    html: room(`<section class="mixer-panel"><div class="mixer-channels"><div class="mixer-channel"><div class="channel-header"><span class="channel-name" data-probe="name">기타</span></div>
      <div class="control-row"><label data-probe="label">볼륨</label><input type="range" data-probe="range"><span data-probe="span">80</span></div>
      <div class="channel-level" data-probe="clevel"><div class="level-meter" data-probe="meter"><div class="level-bar" data-probe="bar"><div class="level-fill high" data-probe="fill"></div></div></div></div>
      <span class="latency-text engine-peer-stats" data-probe="stats">12ms<span class="peer-relay-hint" data-probe="hint"> (릴레이)</span></span></div></div></section>`),
  },
  {
    name: '내 연주자 항목 미니 미터',
    html: room(`<aside class="performers-panel"><div class="performer-item active"><div class="performer-info"><span class="performer-name" data-probe="name">나</span><span class="peer-relay-hint" data-probe="hint">릴레이</span></div><div class="level-meter mini" data-probe="meter"><div class="level-bar" data-probe="bar"><div class="level-fill" data-probe="fill"></div></div></div></div></aside>`),
  },
  {
    name: '헤더 메타 줄',
    html: room(`<header class="live-header"><div class="live-title-area"><h1>방</h1><div class="live-meta"><span class="ui-badge live-badge" data-probe="badge">● LIVE</span><span class="viewer-count" data-probe="viewers">👁 3명</span><span class="host-info" data-probe="host">👑 a</span></div></div><div class="live-header-right"><button class="header-btn" data-probe="hbtn">정보</button></div></header>`),
  },
  {
    name: '빈 녹음 목록·오류 상태',
    html: room(`<div class="recordings-list"><div class="no-recordings"><p data-probe="p">없음</p><small data-probe="small">안내</small></div></div><div class="error-state"><h3 data-probe="h3">오류</h3><p data-probe="ep">설명</p><button data-probe="eb">다시</button></div>`),
  },
  {
    name: '메트로놈 패널',
    html: room(`<section class="metronome-panel"><div class="metronome-field"><label>BPM</label><input type="number" data-probe="input"></div><span class="metronome-status" data-probe="status">정지</span></section>`),
  },
]
