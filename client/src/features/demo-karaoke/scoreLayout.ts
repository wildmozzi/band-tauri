/**
 * 악보 표시(ScoreDisplay.tsx, 데모)의 페이지 분할·커서 계산 — OSMD·React와 분리 (P3-2 ③-4, vitest로 검증).
 * 악보 줄(system)은 렌더된 SVG의 박스(y·height)로만 다룬다.
 */

/** A4 비율: 210mm x 297mm = 1:1.414 — 너비 기준 페이지 높이 */
export const A4_RATIO = 1.414

export function a4HeightFor(width: number): number {
  return width * A4_RATIO
}

export interface SystemBox {
  y: number
  height: number
}

/**
 * 줄 분할: 페이지 시작 y부터 이 줄 아래끝까지(줄 사이 간격 포함)가 페이지 높이를 넘으면 이 줄에서 새 페이지.
 * 첫 페이지는 0에서 시작하고, 첫 줄은 커도 첫 페이지에 둔다. 딱 맞으면(=) 같은 페이지.
 */
export function pageBreaksByBottom(systems: SystemBox[], pageHeight: number): number[] {
  const breaks: number[] = [0]
  systems.forEach((system, index) => {
    const pageStartY = breaks[breaks.length - 1]
    if (index > 0 && system.y + system.height - pageStartY > pageHeight) {
      breaks.push(system.y)
    }
  })
  return breaks
}

/** 줄을 못 찾았을 때: 전체 높이를 페이지 높이로 나눈 페이지 수(올림, 최소 1) */
export function fallbackPageCount(contentHeight: number, pageHeight: number): number {
  if (!(pageHeight > 0) || !(contentHeight > 0)) return 1
  return Math.max(1, Math.ceil(contentHeight / pageHeight))
}

/**
 * SVG 사용자 좌표 → CSS px 배율. OSMD는 SVG를 CSS 크기(10·단위·zoom)로 잡고 viewBox를 크기÷zoom으로 둔다 —
 * 그래서 getBBox() 값은 CSS px ÷ zoom이다. 렌더된 너비 ÷ viewBox 너비로 구하고, 못 구하면 1(viewBox 없음 = 같은 단위).
 */
export function userToCssScale(renderedWidth: number, viewBoxWidth: number): number {
  if (!(renderedWidth > 0) || !(viewBoxWidth > 0)) return 1
  return renderedWidth / viewBoxWidth
}

/** 줄 박스를 배율 k로 변환(사용자 좌표 → CSS px) */
export function scaleBoxes(systems: SystemBox[], k: number): SystemBox[] {
  return systems.map(s => ({ y: s.y * k, height: s.height * k }))
}

export interface PageLayout {
  /** 컨테이너(보이는 창) 높이, CSS px = 컨테이너 너비 기준 A4 */
  pageHeight: number
  /** 페이지별 시작 y(CSS px, SVG 위끝 기준) — translateY에 그대로 쓴다. 길이가 페이지 수 */
  breaks: number[]
}

/**
 * 페이지 레이아웃 — 첫 렌더와 리사이즈가 같은 규칙을 쓴다(P3-13). **입력은 모두 CSS px**(줄 박스는 scaleBoxes로 변환해 넘긴다).
 * 컨테이너 높이 = 컨테이너 너비 기준 A4, 악보가 실제로 보이는 높이 = 그 높이 − verticalInset(위아래 패딩, border-box).
 * 분할은 보이는 높이 기준 줄 아래끝 규칙(pageBreaksByBottom). 줄을 못 찾으면 전체 높이를 균등 분할.
 * 너비가 0 이하(숨김 등)거나 보이는 높이가 0 이하면 한 페이지.
 */
export function computePageLayout(
  systems: SystemBox[],
  containerWidth: number,
  contentHeight: number,
  verticalInset = 0,
): PageLayout {
  const pageHeight = a4HeightFor(Math.max(0, containerWidth))
  const visible = pageHeight - Math.max(0, verticalInset)
  if (!(visible > 0)) return { pageHeight, breaks: [0] }
  if (systems.length > 0) {
    const sorted = [...systems].sort((a, b) => a.y - b.y)
    return { pageHeight, breaks: pageBreaksByBottom(sorted, visible) }
  }
  const count = fallbackPageCount(contentHeight, visible)
  return { pageHeight, breaks: Array.from({ length: count }, (_, i) => i * visible) }
}

/** 플레이어 마디 번호 → 악보 마디 목록 안 인덱스(0..count-1로 자름) */
export function clampMeasureIndex(index: number, count: number): number {
  let target = index
  if (target >= count) target = count - 1
  if (target < 0) target = 0
  return target
}

/** 커서 y가 속한 페이지: 시작 y가 (커서 y + margin) 이하인 마지막 페이지(없으면 0) */
export function pageForY(pageBreaks: number[], y: number, margin = 50): number {
  for (let i = pageBreaks.length - 1; i >= 0; i--) {
    if (pageBreaks[i] <= y + margin) return i
  }
  return 0
}

/**
 * 렌더된 SVG에서 악보 줄(system) 요소 찾기 — 여러 셀렉터를 차례로 시도
 * 1) osmd_system으로 시작하는 id 2) vf-stave 클래스(VexFlow) 3) 'svg > g > g' 중 높이 50 초과·y(10px 반올림)가 다른 것, y 순
 */
export function findScoreSystems(svg: Element): Element[] {
  // 시도 1: osmd_system으로 시작하는 ID
  let systems: Element[] = Array.from(svg.querySelectorAll('g[id^="osmd_system"]'))

  // 시도 2: vf-stave 클래스 (VexFlow 기반)
  if (systems.length === 0) {
    systems = Array.from(svg.querySelectorAll('g.vf-stave'))
  }

  // 시도 3: 최상위 g 요소들 중 y 좌표가 다른 것들 (악보 줄로 추정)
  if (systems.length === 0) {
    const allGroups = Array.from(svg.querySelectorAll('svg > g > g'))
    const yPositions = new Map<number, Element>()

    allGroups.forEach(g => {
      try {
        const bbox = (g as SVGGraphicsElement).getBBox()
        const roundedY = Math.round(bbox.y / 10) * 10 // 10px 단위로 반올림
        if (bbox.height > 50 && !yPositions.has(roundedY)) { // 높이가 50px 이상인 것만
          yPositions.set(roundedY, g)
        }
      } catch {
        // getBBox 실패 시 무시
      }
    })

    systems = Array.from(yPositions.values()).sort((a, b) => {
      const bboxA = (a as SVGGraphicsElement).getBBox()
      const bboxB = (b as SVGGraphicsElement).getBBox()
      return bboxA.y - bboxB.y
    })
  }

  return systems
}

/** 줄 요소 → 박스 */
export function systemBoxes(systems: Element[]): SystemBox[] {
  return systems.map(system => {
    const bbox = (system as SVGGraphicsElement).getBBox()
    return { y: bbox.y, height: bbox.height }
  })
}
