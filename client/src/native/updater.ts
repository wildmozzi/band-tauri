/**
 * 데스크톱 자동 업데이트 (P2-1, tauri-plugin-updater v2).
 *
 * - 엔드포인트: GitHub Releases `latest.json` (tauri.conf.json plugins.updater)
 * - 앱 시작 시 조용히 확인하고, 설정 화면에서 수동 확인한다.
 * - 연주 중(엔진 동작 중 또는 방 참여 중)에는 설치를 미루고 알림만 한다. Windows에서는 install()이
 *   NSIS를 띄우고 곧바로 프로세스를 종료하므로, 연주 도중 실행하면 합주가 끊긴다.
 * - 다운로드와 설치를 나눈다: download → 연주 여부 재확인 → install. 다운로드 중에 연주를 시작하면
 *   받은 패키지는 그대로 두고 보류한다(TOCTOU 방지, PR #4 리뷰 M1).
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
  | { kind: 'available'; version: string; notes?: string; ready: boolean }
  /** 연주 중이라 설치를 미룸. ready: 패키지를 이미 받아 둠. 연주가 끝나면 available로 다시 알린다. */
  | { kind: 'deferred'; version: string; notes?: string; ready: boolean }
  | { kind: 'downloading'; version: string; downloaded: number; total: number | null }
  | { kind: 'installing'; version: string }
  | { kind: 'error'; message: string }

/** plugin-updater `Update` 중 우리가 쓰는 부분 */
export interface UpdateHandle {
  version: string
  body?: string
  download(onEvent?: (e: DownloadEvent) => void): Promise<void>
  install(): Promise<void>
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
  /**
   * 방 참여 여부 (엔진 시작 전 대기 중도 연주 중으로 본다).
   * 참여 → 해제로 바뀌고 보류된 업데이트가 있으면 다시 알린다.
   */
  setInRoom(inRoom: boolean): void
}

export function createUpdater(deps: UpdaterDeps): Updater {
  let status: UpdateStatus = { kind: 'idle' }
  let pending: UpdateHandle | null = null
  let ready = false
  let inRoom = false
  const listeners = new Set<() => void>()

  const set = (next: UpdateStatus) => {
    status = next
    listeners.forEach((l) => l())
  }
  const busy = () => status.kind === 'checking' || status.kind === 'downloading' || status.kind === 'installing'
  const isPerforming = async () => inRoom || (await deps.isEngineRunning())

  function offerState(update: UpdateHandle, performing: boolean): UpdateStatus {
    return { kind: performing ? 'deferred' : 'available', version: update.version, notes: update.body, ready }
  }

  return {
    getStatus: () => status,
    subscribe(listener) {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },

    async checkForUpdate(opts) {
      if (busy()) return
      const silent = opts?.silent ?? false
      const prev = status
      if (!silent) set({ kind: 'checking' })
      try {
        const update = await deps.check()
        if (update) {
          // 같은 버전을 이미 받아 뒀으면 그 핸들(다운로드한 바이트)을 유지한다.
          if (!pending || pending.version !== update.version || !ready) {
            pending = update
            ready = false
          }
          set(offerState(pending, await isPerforming()))
        } else {
          pending = null
          ready = false
          set(silent ? prev : { kind: 'upToDate' })
        }
      } catch (err) {
        set(silent ? prev : { kind: 'error', message: engineErrorMessage(err) })
      }
    },

    async installUpdate() {
      const update = pending
      if (!update || busy()) return
      if (await isPerforming()) {
        set(offerState(update, true))
        return
      }

      if (!ready) {
        let downloaded = 0
        let total: number | null = null
        set({ kind: 'downloading', version: update.version, downloaded, total })
        try {
          await update.download((e) => {
            if (e.event === 'Started') total = e.data.contentLength ?? null
            else if (e.event === 'Progress') downloaded += e.data.chunkLength
            else return
            set({ kind: 'downloading', version: update.version, downloaded, total })
          })
        } catch (err) {
          set({ kind: 'error', message: `업데이트 다운로드 실패: ${engineErrorMessage(err)}` })
          return
        }
        ready = true
      }

      // 다운로드하는 동안 연주를 시작했을 수 있다 — 설치(=앱 종료) 직전에 다시 확인.
      if (await isPerforming()) {
        set(offerState(update, true))
        return
      }

      set({ kind: 'installing', version: update.version })
      try {
        await update.install()
        // Windows는 install()이 NSIS를 띄우고 앱을 종료하므로 보통 여기까지 오지 않는다.
        await deps.relaunch()
      } catch (err) {
        set({ kind: 'error', message: `업데이트 설치 실패: ${engineErrorMessage(err)}` })
      }
    },

    dismiss() {
      if (busy()) return
      set({ kind: 'idle' })
    },

    setInRoom(next) {
      const left = inRoom && !next
      inRoom = next
      if (!pending || busy()) return
      if (next && status.kind === 'available') {
        set(offerState(pending, true))
      } else if (left) {
        // 방을 나왔다 — 보류(또는 "나중에"로 닫힌) 업데이트를 다시 알린다. 엔진은 install 직전에 재확인.
        set(offerState(pending, false))
      }
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
