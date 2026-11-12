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
 * 첫 렌더 분할: 페이지 시작 y부터 이 줄 아래끝까지가 A4 높이를 넘으면 이 줄에서 새 페이지.
 * 첫 페이지는 0에서 시작. lastBottom은 마지막 줄 아래끝(로그용).
 */
export function pageBreaksByBottom(systems: SystemBox[], a4Height: number): { breaks: number[]; lastBottom: number } {
  const breaks: number[] = [0] // 첫 페이지 시작
  let lastBottom = 0
  systems.forEach((system, index) => {
    const bottom = system.y + system.height
    // 현재 페이지에 이 system을 추가하면 A4 높이를 넘는지 확인
    const pageStartY = breaks[breaks.length - 1]
    if (index > 0 && bottom - pageStartY > a4Height) {
      // 새 페이지 시작
      breaks.push(system.y)
    }
    lastBottom = bottom
  })
  return { breaks, lastBottom }
}

/** 리사이즈 분할: 줄 높이를 쌓아 A4 높이를 넘으면 이 줄에서 새 페이지(줄 사이 간격은 세지 않음) */
export function pageBreaksByHeight(systems: SystemBox[], a4Height: number): number[] {
  const breaks: number[] = [0]
  let currentPageHeight = 0
  systems.forEach((system, index) => {
    if (index > 0 && currentPageHeight + system.height > a4Height) {
      breaks.push(system.y)
      currentPageHeight = system.height
    } else {
      currentPageHeight += system.height
    }
  })
  return breaks
}

/** 줄을 못 찾았을 때: 전체 높이를 A4 높이로 나눈 페이지 수(올림) */
export function fallbackPageCount(svgHeight: number, a4Height: number): number {
  return Math.ceil(svgHeight / a4Height)
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
export function findScoreSystems(svg: Element, log: (label: string, count: number) => void = () => {}): Element[] {
  // 시도 1: osmd_system으로 시작하는 ID
  let systems: Element[] = Array.from(svg.querySelectorAll('g[id^="osmd_system"]'))
  log('osmd_system', systems.length)

  // 시도 2: vf-stave 클래스 (VexFlow 기반)
  if (systems.length === 0) {
    systems = Array.from(svg.querySelectorAll('g.vf-stave'))
    log('vf-stave', systems.length)
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
    log('by position', systems.length)
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
