// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import {
  A4_RATIO,
  a4HeightFor,
  clampMeasureIndex,
  computePageLayout,
  fallbackPageCount,
  findScoreSystems,
  pageBreaksByBottom,
  pageForY,
  type SystemBox,
} from './scoreLayout'

/** 높이 h, 간격 gap인 줄 n개 (y=top부터) */
function rows(n: number, h: number, gap: number, top = 0): SystemBox[] {
  return Array.from({ length: n }, (_, i) => ({ y: top + i * (h + gap), height: h }))
}

describe('scoreLayout', () => {
  it('a4HeightFor: 너비 × 1.414', () => {
    expect(a4HeightFor(1000)).toBeCloseTo(1414)
  })

  it('pageBreaksByBottom: 페이지 시작부터 줄 아래끝(간격 포함)이 넘으면 새 페이지', () => {
    const systems = [
      { y: 0, height: 100 },
      { y: 120, height: 100 },
      { y: 240, height: 100 }, // 아래끝 340 > 300 → 새 페이지(240)
      { y: 360, height: 100 }, // 460-240=220 ≤ 300
      { y: 480, height: 100 }, // 580-240=340 > 300 → 새 페이지(480)
    ]
    expect(pageBreaksByBottom(systems, 300)).toEqual([0, 240, 480])
    // 첫 줄은 커도 첫 페이지에 둔다
    expect(pageBreaksByBottom([{ y: 0, height: 999 }], 300)).toEqual([0])
    expect(pageBreaksByBottom([], 300)).toEqual([0])
  })

  it('pageBreaksByBottom 경계: 딱 맞으면 같은 페이지, 한 픽셀이라도 넘으면 새 페이지', () => {
    const systems = [{ y: 0, height: 100 }, { y: 200, height: 100 }] // 둘째 아래끝 300
    expect(pageBreaksByBottom(systems, 300)).toEqual([0])
    expect(pageBreaksByBottom(systems, 299.99)).toEqual([0, 200])
  })

  it('computePageLayout: 기준은 컨테이너 너비, 분할은 아래끝 규칙', () => {
    const systems = rows(5, 100, 20) // y 0,120,240,360,480
    const width = 300 / A4_RATIO
    const layout = computePageLayout(systems, width, 9999)
    expect(layout.pageHeight).toBeCloseTo(300)
    expect(layout.breaks).toEqual([0, 240, 480])
  })

  it('computePageLayout: 줄 순서가 섞여도 y 순으로 분할', () => {
    const systems = rows(5, 100, 20).reverse()
    expect(computePageLayout(systems, 300 / A4_RATIO, 0).breaks).toEqual([0, 240, 480])
  })

  it('computePageLayout 경계 폭: 페이지가 줄 아래끝에 딱 맞는 너비 전후', () => {
    // 줄 2개(아래끝 300) — 페이지 높이 300이면 1쪽, 그보다 조금이라도 좁으면 2쪽
    const systems = [{ y: 0, height: 100 }, { y: 200, height: 100 }]
    const exact = 300 / A4_RATIO
    expect(computePageLayout(systems, exact, 0).breaks).toHaveLength(1)
    expect(computePageLayout(systems, exact - 0.01, 0).breaks).toEqual([0, 200])
    expect(computePageLayout(systems, exact + 0.01, 0).breaks).toHaveLength(1)
  })

  it('computePageLayout: 너비가 늘수록 페이지 수는 늘지 않는다(조금 바꿔도 튀지 않음)', () => {
    const systems = rows(30, 90, 25, 60)
    let prev = Infinity
    for (let width = 200; width <= 1600; width += 1) {
      const pages = computePageLayout(systems, width, 0).breaks.length
      expect(pages).toBeLessThanOrEqual(prev)
      prev = pages
    }
  })

  it('computePageLayout: 첫 렌더·리사이즈가 같은 입력이면 같은 결과(단일 규칙)', () => {
    const systems = rows(12, 110, 30, 80)
    expect(computePageLayout(systems, 640, 0)).toEqual(computePageLayout(systems.map(s => ({ ...s })), 640, 0))
  })

  it('computePageLayout: 줄이 없으면 높이 균등 분할, 너비 0이면 한 페이지', () => {
    const width = 300 / A4_RATIO
    const layout = computePageLayout([], width, 650)
    expect(layout.breaks).toHaveLength(3)
    expect(layout.breaks[1]).toBeCloseTo(300)
    expect(layout.breaks[2]).toBeCloseTo(600)
    expect(computePageLayout([], width, 0).breaks).toEqual([0])
    expect(computePageLayout(rows(3, 100, 20), 0, 1000).breaks).toEqual([0])
    expect(computePageLayout(rows(3, 100, 20), -5, 1000).breaks).toEqual([0])
  })

  it('fallbackPageCount: 올림, 최소 1', () => {
    expect(fallbackPageCount(301, 300)).toBe(2)
    expect(fallbackPageCount(300, 300)).toBe(1)
    expect(fallbackPageCount(0, 300)).toBe(1)
    expect(fallbackPageCount(500, 0)).toBe(1)
  })

  it('clampMeasureIndex: 0..count-1', () => {
    expect(clampMeasureIndex(5, 3)).toBe(2)
    expect(clampMeasureIndex(-1, 3)).toBe(0)
    expect(clampMeasureIndex(1, 3)).toBe(1)
  })

  it('pageForY: 시작 y ≤ y+margin인 마지막 페이지', () => {
    const breaks = [0, 500, 1000]
    expect(pageForY(breaks, 449)).toBe(0)
    expect(pageForY(breaks, 450)).toBe(1)
    expect(pageForY(breaks, 2000)).toBe(2)
    expect(pageForY(breaks, 960, 10)).toBe(1)
  })

  it('findScoreSystems: osmd_system id를 먼저, 없으면 vf-stave', () => {
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
    svg.innerHTML = '<g><g class="vf-stave"></g><g id="osmd_system_1"></g><g id="osmd_system_2"></g></g>'
    expect(findScoreSystems(svg).map(e => e.id)).toEqual(['osmd_system_1', 'osmd_system_2'])

    const svg2 = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
    svg2.innerHTML = '<g><g class="vf-stave"></g></g>'
    expect(findScoreSystems(svg2)).toHaveLength(1)
  })
})
