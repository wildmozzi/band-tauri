/**
 * 가짜 Tauri IPC (데스크톱 앱 화면을 웹 e2e로 검사할 때만, 테스트가 직접 install).
 * `@tauri-apps/api/core`의 isTauri()는 `globalThis.isTauri`, invoke는 `window.__TAURI_INTERNALS__.invoke`를 본다.
 * 엔진 커맨드는 `responses`의 값(없으면 null)을 돌려주고, 부른 커맨드 이름을 `window.__e2eTauriCalls`에 남긴다.
 * 실제 엔진·Rust 코드는 돌지 않는다 — 엔진 통계를 화면이 어떻게 보여 주는지만 본다.
 */
import type { Page } from '@playwright/test'
import type { DriverKind, EngineStats, LocalDescriptor } from '../../src/native/engineApi'

export interface FakeTauriResponses {
  engine_start?: LocalDescriptor
  engine_stats?: EngineStats
  /** 이 빌드의 지원 드라이버(P2-7). 빼면 null → 옛 엔진처럼 모든 드라이버를 보인다 */
  engine_supported_drivers?: DriverKind[]
  [cmd: string]: unknown
}

export async function installFakeTauri(page: Page, responses: FakeTauriResponses) {
  await page.addInitScript((canned: Record<string, unknown>) => {
    const calls: string[] = []
    const callbacks = new Map<number, (v: unknown) => void>()
    let nextId = 1
    const w = window as unknown as Record<string, unknown>
    w.__e2eTauriCalls = calls
    w.isTauri = true
    w.__TAURI_INTERNALS__ = {
      invoke: async (cmd: string) => {
        calls.push(cmd)
        return Object.prototype.hasOwnProperty.call(canned, cmd) ? structuredClone(canned[cmd]) : null
      },
      transformCallback: (cb: (v: unknown) => void) => {
        const id = nextId++
        callbacks.set(id, cb)
        return id
      },
      unregisterCallback: (id: number) => callbacks.delete(id),
      convertFileSrc: (p: string) => p,
      metadata: { currentWindow: { label: 'main' }, currentWebview: { windowLabel: 'main', label: 'main' } },
    }
  }, responses as Record<string, unknown>)
}

/** 지금까지 앱이 부른 Tauri 커맨드 */
export function tauriCalls(page: Page): Promise<string[]> {
  return page.evaluate(() => [...((window as unknown as { __e2eTauriCalls?: string[] }).__e2eTauriCalls ?? [])])
}
