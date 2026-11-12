// @vitest-environment jsdom
import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createQueryWrapper } from '../../shared/test/queryWrapper'
import type { StemSeparation } from './musicApi'

const api = vi.hoisted(() => ({ fetchStemHistory: vi.fn(), deleteStemSeparation: vi.fn() }))
vi.mock('./musicApi', () => api)

import {
  clearStemHistory,
  fetchFreshStemHistory,
  stemKeys,
  useDeleteStemSeparationMutation,
  useStemHistoryQuery,
} from './stemQueries'

const sep = (id: string) => ({ id, original_filename: `${id}.mp3` }) as StemSeparation
const ids = (v: unknown) => (v as StemSeparation[]).map(s => s.id)

afterEach(() => {
  vi.resetAllMocks()
  vi.restoreAllMocks()
})

describe('stems queries', () => {
  it('열기 전에는 조회하지 않고, 한 번 읽은 뒤에는 다시 열어도 재조회하지 않는다', async () => {
    api.fetchStemHistory.mockResolvedValue([sep('a')])
    const { wrapper } = createQueryWrapper()
    const { result, rerender } = renderHook(({ on }) => useStemHistoryQuery('u1', on), {
      wrapper,
      initialProps: { on: false },
    })
    expect(api.fetchStemHistory).not.toHaveBeenCalled()

    rerender({ on: true })
    await waitFor(() => expect(result.current.data).toEqual([sep('a')]))
    rerender({ on: false })
    rerender({ on: true })
    expect(api.fetchStemHistory).toHaveBeenCalledTimes(1)
  })

  it('조회 실패는 빈 목록 성공으로 캐시하지 않는다(다시 열면 재조회)', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {})
    api.fetchStemHistory.mockRejectedValueOnce(new Error('net')).mockResolvedValueOnce([sep('a')])
    const { wrapper } = createQueryWrapper()
    const { result, rerender } = renderHook(({ on }) => useStemHistoryQuery('u1', on), {
      wrapper,
      initialProps: { on: true },
    })
    await waitFor(() => expect(result.current.isError).toBe(true))
    expect(result.current.data).toBeUndefined()

    await act(async () => { await result.current.refetch() })
    rerender({ on: true })
    await waitFor(() => expect(result.current.data).toEqual([sep('a')]))
  })

  it('중복 검사 조회가 실패하면 기존 캐시를 지우지 않고 캐시값을 돌려준다', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {})
    api.fetchStemHistory.mockRejectedValue(new Error('net'))
    const { client } = createQueryWrapper()
    client.setQueryData(stemKeys.history('u1'), [sep('kept')])

    expect(await fetchFreshStemHistory(client, 'u1')).toEqual([sep('kept')])
    expect(client.getQueryData(stemKeys.history('u1'))).toEqual([sep('kept')])
  })

  it('중복 검사용 조회는 캐시가 있어도 서버에서 다시 읽는다', async () => {
    api.fetchStemHistory.mockResolvedValue([sep('new')])
    const { client } = createQueryWrapper()
    client.setQueryData(stemKeys.history('u1'), [sep('old')])

    expect(await fetchFreshStemHistory(client, 'u1')).toEqual([sep('new')])
    expect(client.getQueryData(stemKeys.history('u1'))).toEqual([sep('new')])
  })

  it('삭제 성공이면 캐시에서 빼고, 실패(false)면 그대로 둔다', async () => {
    const { wrapper, client } = createQueryWrapper()
    client.setQueryData(stemKeys.history('u1'), [sep('a'), sep('b')])
    const { result } = renderHook(() => useDeleteStemSeparationMutation('u1'), { wrapper })

    api.deleteStemSeparation.mockResolvedValueOnce(false)
    await act(() => result.current.mutateAsync('a'))
    expect(ids(client.getQueryData(stemKeys.history('u1')))).toEqual(['a', 'b'])

    api.deleteStemSeparation.mockResolvedValueOnce(true)
    await act(() => result.current.mutateAsync('a'))
    expect(ids(client.getQueryData(stemKeys.history('u1')))).toEqual(['b'])
  })

  it('로그아웃 시 모든 사용자의 히스토리 캐시를 지운다', () => {
    const { client } = createQueryWrapper()
    client.setQueryData(stemKeys.history('u1'), [sep('a')])
    client.setQueryData(stemKeys.history(null), [])
    clearStemHistory(client)
    expect(client.getQueryData(stemKeys.history('u1'))).toBeUndefined()
    expect(client.getQueryData(stemKeys.history(null))).toBeUndefined()
  })
})
