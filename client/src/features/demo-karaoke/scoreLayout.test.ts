// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import {
  a4HeightFor,
  clampMeasureIndex,
  fallbackPageCount,
  findScoreSystems,
  pageBreaksByBottom,
  pageBreaksByHeight,
  pageForY,
} from './scoreLayout'

describe('scoreLayout', () => {
  it('a4HeightFor: 너비 × 1.414', () => {
    expect(a4HeightFor(1000)).toBeCloseTo(1414)
  })

  it('pageBreaksByBottom: 페이지 시작부터 줄 아래끝이 A4를 넘으면 새 페이지', () => {
    const systems = [
      { y: 0, height: 100 },
      { y: 120, height: 100 },
      { y: 240, height: 100 }, // 아래끝 340 > 300 → 새 페이지(240)
      { y: 360, height: 100 }, // 460-240=220 ≤ 300
      { y: 480, height: 100 }, // 580-240=340 > 300 → 새 페이지(480)
    ]
    expect(pageBreaksByBottom(systems, 300)).toEqual({ breaks: [0, 240, 480], lastBottom: 580 })
    // 첫 줄은 커도 첫 페이지에 둔다
    expect(pageBreaksByBottom([{ y: 0, height: 999 }], 300).breaks).toEqual([0])
    expect(pageBreaksByBottom([], 300)).toEqual({ breaks: [0], lastBottom: 0 })
  })

  it('pageBreaksByHeight: 줄 높이만 쌓아 넘으면 새 페이지(간격 무시)', () => {
    const systems = [
      { y: 0, height: 100 },
      { y: 120, height: 100 },
      { y: 240, height: 100 }, // 300 ≤ 300 → 같은 페이지
      { y: 360, height: 100 }, // 400 > 300 → 새 페이지(360)
    ]
    expect(pageBreaksByHeight(systems, 300)).toEqual([0, 360])
  })

  it('fallbackPageCount: 올림', () => {
    expect(fallbackPageCount(301, 300)).toBe(2)
    expect(fallbackPageCount(300, 300)).toBe(1)
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
    const seen: string[] = []
    expect(findScoreSystems(svg, (label, n) => seen.push(`${label}:${n}`)).map(e => e.id)).toEqual(['osmd_system_1', 'osmd_system_2'])
    expect(seen).toEqual(['osmd_system:2'])

    const svg2 = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
    svg2.innerHTML = '<g><g class="vf-stave"></g></g>'
    expect(findScoreSystems(svg2)).toHaveLength(1)
  })
})
