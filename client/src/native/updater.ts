/**
 * 데스크톱 자동 업데이트 (P2-1, tauri-plugin-updater v2).
 *
 * - 엔드포인트: GitHub Releases `latest.json` (tauri.conf.json plugins.updater)
 * - 앱 시작 시 조용히 확인하고, 설정 화면에서 수동 확인한다.
 * - 연주 중(엔진 동작 중)에는 설치를 미루고 알림만 한다. 설치는 NSIS가 앱을 종료시키므로
 *   연주 도중 실행하면 합주가 끊긴다.
 *
 * 플러그인 모듈은 import만으로 Tauri 전역을 건드리지 않으며, 모든 호출은 isNativeApp() 가드 뒤에 있다.
 */
import { useSyncExternalStore } from 'react'
import { check } from '@tauri-apps/plugin-updater'
import { relaunch } from '@tauri-apps/plugin-process'
import { engineErrorMessage, engineStats, isNativeApp } from './engineApi'

export type UpdateStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'upToDate' }
  | { kind: 'available'; version: string; notes?: string }
  /** 연주 중이라 설치를 미룸 — 엔진을 멈춘 뒤 다시 설치를 누르면 진행 */
  | { kind: 'deferred'; version: string; notes?: string }
  | { kind: 'downloading'; version: string; downloaded: number; total: number | null }
  | { kind: 'installing'; version: string }
  | { kind: 'error'; message: string }

/** plugin-updater `Update` 중 우리가 쓰는 부분 */
export interface UpdateHandle {
  version: string
  body?: string
  downloadAndInstall(onEvent?: (e: DownloadEvent) => void): Promise<void>
}

export type DownloadEvent =
  | { event: 'Started'; data: { contentLength?: number } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished' }

export interface UpdaterDeps {
  check(): Promise<UpdateHandle | null>
  relaunch(): Promise<void>
  /** 엔진(연주/테스트)이 돌고 있는지. 판정 실패 시 true로 취급(안전 쪽) */
  isEngineRunning(): Promise<boolean>
}

export interface Updater {
  getStatus(): UpdateStatus
  subscribe(listener: () => void): () => void
  /** silent: 시작 시 자동 확인. 실패·최신이면 상태를 idle로 남겨 UI를 띄우지 않는다. */
  checkForUpdate(opts?: { silent?: boolean }): Promise<void>
  installUpdate(): Promise<void>
  dismiss(): void
}

export function createUpdater(deps: UpdaterDeps): Updater {
  let status: UpdateStatus = { kind: 'idle' }
  let pending: UpdateHandle | null = null
  const listeners = new Set<() => void>()

  const set = (next: UpdateStatus) => {
    status = next
    listeners.forEach((l) => l())
  }
  const busy = () => status.kind === 'checking' || status.kind === 'downloading' || status.kind === 'installing'

  async function offer(update: UpdateHandle) {
    pending = update
    const running = await deps.isEngineRunning()
    set({ kind: running ? 'deferred' : 'available', version: update.version, notes: update.body })
  }

  return {
    getStatus: () => status,
    subscribe(listener) {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },

    async checkForUpdate(opts) {
      if (busy()) return
      const silent = opts?.silent ?? false
      const prev = status
      if (!silent) set({ kind: 'checking' })
      try {
        const update = await deps.check()
        if (update) {
          await offer(update)
        } else {
          pending = null
          set(silent ? prev : { kind: 'upToDate' })
        }
      } catch (err) {
        set(silent ? prev : { kind: 'error', message: engineErrorMessage(err) })
      }
    },

    async installUpdate() {
      const update = pending
      if (!update || busy()) return
      if (await deps.isEngineRunning()) {
        set({ kind: 'deferred', version: update.version, notes: update.body })
        return
      }
      let downloaded = 0
      let total: number | null = null
      set({ kind: 'downloading', version: update.version, downloaded, total })
      try {
        await update.downloadAndInstall((e) => {
          if (e.event === 'Started') {
            total = e.data.contentLength ?? null
          } else if (e.event === 'Progress') {
            downloaded += e.data.chunkLength
          } else {
            set({ kind: 'installing', version: update.version })
            return
          }
          set({ kind: 'downloading', version: update.version, downloaded, total })
        })
        set({ kind: 'installing', version: update.version })
        // Windows는 NSIS 설치 프로그램이 앱을 종료하므로 보통 여기까지 오지 않는다.
        await deps.relaunch()
      } catch (err) {
        set({ kind: 'error', message: `업데이트 설치 실패: ${engineErrorMessage(err)}` })
      }
    },

    dismiss() {
      if (busy()) return
      set({ kind: 'idle' })
    },
  }
}

// ============================================================================
// 앱 전역 인스턴스
// ============================================================================

const tauriDeps: UpdaterDeps = {
  check: () => check(),
  relaunch: () => relaunch(),
  async isEngineRunning() {
    try {
      return (await engineStats()).running
    } catch {
      return true
    }
  },
}

export const appUpdater: Updater = createUpdater(tauriDeps)

/** 데스크톱 앱에서만 업데이트가 의미 있다 (웹 관람자는 항상 최신) */
export function isUpdaterAvailable(): boolean {
  return isNativeApp()
}

export function useUpdateStatus(): UpdateStatus {
  return useSyncExternalStore(appUpdater.subscribe, appUpdater.getStatus, appUpdater.getStatus)
}
