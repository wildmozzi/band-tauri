import { describe, expect, it } from 'vitest'
import { ALL_PLAN_FEATURES, PLAN_COMPARISON, PLANNED_FEATURES, plannedFeaturesFor } from './planFeatures'
import { PLAN_LIMITS } from './usePremium'
import { MAX_PERFORMERS } from '../../native/engineApi'
import { ROOM_CAPACITY_MAX } from '../rooms/roomModel'

describe('요금제 문구 (P3-12a)', () => {
  it('인원·비공개 방 한도는 요금제와 무관하게 실제 규칙과 같다', () => {
    for (const plan of ['free', 'standard', 'pro'] as const) {
      expect(PLAN_LIMITS[plan]).toEqual({
        maxPerformers: MAX_PERFORMERS,
        maxRoomCapacity: ROOM_CAPACITY_MAX,
        canCreatePrivateRooms: true,
      })
    }
  })

  it('지금 되는 기능 목록에 실제 인원 규칙이 들어간다', () => {
    expect(ALL_PLAN_FEATURES).toContain(`연주 최대 ${MAX_PERFORMERS}명 (데스크톱 앱)`)
    expect(ALL_PLAN_FEATURES).toContain(`합주실 정원 관람 포함 최대 ${ROOM_CAPACITY_MAX}명`)
  })

  it('준비 중 기능은 지금 되는 기능 목록에 섞이지 않는다', () => {
    for (const f of PLANNED_FEATURES) expect(ALL_PLAN_FEATURES).not.toContain(f.name)
  })

  it('Pro 준비 중 목록은 Standard 것을 포함한다', () => {
    const standard = plannedFeaturesFor('standard')
    const pro = plannedFeaturesFor('pro')
    expect(standard.length).toBeGreaterThan(0)
    for (const f of standard) expect(pro).toContain(f)
    expect(pro.length).toBe(PLANNED_FEATURES.length)
  })

  it('비교표: 지금 되는 행은 세 요금제가 같고, 준비 중 행은 체크(✅)로 표시하지 않는다', () => {
    const planned = new Set(PLANNED_FEATURES.map(f => f.name))
    for (const row of PLAN_COMPARISON) {
      if (planned.has(row.name)) {
        expect([row.free, row.standard, row.pro]).not.toContain(true)
      } else {
        expect(row.standard).toEqual(row.free)
        expect(row.pro).toEqual(row.free)
      }
    }
  })
})
