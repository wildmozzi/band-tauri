import { useCallback, useEffect, useRef, useState } from 'react'
import { OpenSheetMusicDisplay } from 'opensheetmusicdisplay'
import {
  clampMeasureIndex,
  computePageLayout,
  findScoreSystems,
  pageForY,
  scaleBoxes,
  systemBoxes,
  type PageLayout,
  userToCssScale,
} from './scoreLayout'

interface ScoreDisplayProps {
  musicXmlUrl?: string
  currentProgress?: number // 0 ~ 1 사이의 진행률
  currentMeasure?: number // [추가] 현재 마디 번호 (0부터 시작)
  onError?: (error: Error) => void
}

/** 렌더 전(악보 없음) 레이아웃 — 한 페이지 */
const INITIAL_LAYOUT: PageLayout = { pageHeight: 1100, breaks: [0] }

/** OSMD autoResize가 다시 그린 뒤 측정하도록 마지막 resize 이벤트 후 기다리는 시간 */
const RESIZE_SETTLE_MS = 500

/**
 * OpenSheetMusicDisplay를 사용한 악보 시각화 컴포넌트
 * 페이지 단위로 표시
 */
export function ScoreDisplay({ musicXmlUrl, currentProgress = 0, currentMeasure = 0, onError }: ScoreDisplayProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const osmdRef = useRef<OpenSheetMusicDisplay | null>(null)
  const [currentPage, setCurrentPage] = useState(0)
  const [layout, setLayout] = useState<PageLayout>(INITIAL_LAYOUT)
  const [svgElement, setSvgElement] = useState<SVGElement | null>(null)
  const [maxRealValue, setMaxRealValue] = useState(0)
  const [measureStarts, setMeasureStarts] = useState<number[]>([])
  const totalPages = layout.breaks.length
  const pageHeight = layout.pageHeight

  // 하이라이트 점프 및 깜빡임 방지를 위한 Ref
  const lastTargetTimeRef = useRef(0)
  const currentProgressRef = useRef(0)
  const currentMeasureRef = useRef(0)
  const lastMeasureIndexRef = useRef(-1)
  // 커서 rAF 루프가 읽는 값 — 루프를 다시 만들지 않도록 ref로 넘긴다(P3-13)
  const pageBreaksRef = useRef<number[]>(INITIAL_LAYOUT.breaks)
  const currentPageRef = useRef(0)
  const svgElementRef = useRef<SVGElement | null>(null)

  // musicXmlUrl 변경 시(새 악보) 상태 및 Ref 초기화
  useEffect(() => {
    lastMeasureIndexRef.current = -1
    lastTargetTimeRef.current = 0
    currentProgressRef.current = 0
    currentMeasureRef.current = 0
  }, [musicXmlUrl])

  // state 대입
  useEffect(() => {
    currentProgressRef.current = currentProgress
    currentMeasureRef.current = currentMeasure
  }, [currentProgress, currentMeasure])

  useEffect(() => {
    pageBreaksRef.current = layout.breaks
    currentPageRef.current = currentPage
    svgElementRef.current = svgElement
  }, [layout, currentPage, svgElement])

  /**
   * 지금 그려진 SVG를 재서 페이지 레이아웃 갱신 — 첫 렌더와 리사이즈가 같은 함수·같은 규칙(computePageLayout).
   * OSMD가 다시 그리면 SVG 요소가 바뀔 수 있어 매번 컨테이너에서 새로 찾는다.
   */
  const measureLayout = useCallback(() => {
    const container = containerRef.current
    if (!container) return
    const svg = container.querySelector('svg')
    // OSMD 재렌더 도중 잠깐 SVG가 없을 수 있다 — 이전 레이아웃을 그대로 두고 다음 측정을 기다린다
    if (!svg) return
    // 단위를 CSS px로 맞춘다: 줄 박스(getBBox)는 SVG 사용자 좌표(CSS px ÷ zoom)라 배율 k를 곱한다
    const rect = svg.getBoundingClientRect() // translateY는 너비·높이에 영향 없음
    const k = userToCssScale(rect.width, svg.viewBox?.baseVal?.width ?? 0)
    const cs = getComputedStyle(container)
    const padY = cs.boxSizing === 'border-box'
      ? (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0)
      : 0
    const next = computePageLayout(
      scaleBoxes(systemBoxes(findScoreSystems(svg)), k),
      container.clientWidth,
      rect.height,
      padY,
    )
    setSvgElement(svg)
    setLayout(next)
    setCurrentPage(p => Math.min(p, next.breaks.length - 1))
    // 분할이 바뀌었으니 다음 프레임에 커서 위치로 페이지를 다시 판정
    lastMeasureIndexRef.current = -1
  }, [])

  useEffect(() => {
    if (!containerRef.current) return

    // OpenSheetMusicDisplay 초기화
    const osmd = new OpenSheetMusicDisplay(containerRef.current, {
      autoResize: true,
      backend: 'svg',
      drawTitle: true,
      drawingParameters: 'compacttight',
      drawPartNames: true,
      drawMeasureNumbers: true,
      drawCredits: true,
      // 페이지 포맷 설정
      pageFormat: 'Endless',
      pageBackgroundColor: '#FFFFFF',
      renderSingleHorizontalStaffline: false,
      // 마디 배치 설정 - XML의 줄바꿈 따르기
      newSystemFromXML: true, // XML의 줄바꿈 사용
      newPageFromXML: false,
    })

    // 줌 레벨 설정 (시독성을 위해 상향)
    osmd.zoom = 0.6

    osmdRef.current = osmd

    return () => {
      if (osmdRef.current) {
        try {
          // 커서가 존재할 때만 숨기기
          const cursor = osmdRef.current.cursor
          if (cursor) {
            cursor.hide()
          }
        } catch {
          // 이미 정리된 상태라면 무시
        }
      }
      osmdRef.current = null
    }
  }, [])

  const prevUrlRef = useRef<string>('')

  useEffect(() => {
    if (!osmdRef.current || !musicXmlUrl) return
    if (prevUrlRef.current === musicXmlUrl) return // 이미 같은 URL이 로드됨
    prevUrlRef.current = musicXmlUrl

    let isCancelled = false

    const loadScore = async () => {
      try {
        setMaxRealValue(0) // 새 악보 로드 시 초기화

        await osmdRef.current!.load(musicXmlUrl)

        if (!isCancelled && osmdRef.current) {
          await osmdRef.current.render()

          // 렌더링 후 커서 설정 재적용 및 표시
          const osmd = osmdRef.current
          // eslint-disable-next-line @typescript-eslint/no-explicit-any -- OSMD 커서 내부 API는 타입 미제공
          const cursor = osmd.cursor as any
          if (cursor) {
            cursor.cursorOptions.color = '#FFFF00EE' // 아주 선명한 노란색
            cursor.cursorOptions.type = 0 // 0: Normal (박스 형태의 기반)
            cursor.cursorOptions.followCursor = true
            cursor.show()
          }

          // 커서 기반으로 마디 데이터 추출 (정확도 보장)
          try {
            if (cursor) {
              cursor.hide()
              cursor.reset()
              const starts: number[] = []
              // eslint-disable-next-line @typescript-eslint/no-explicit-any -- OSMD 커서 내부 API는 타입 미제공
              let lastMeasureObj: any = null
              let lastValue = 0

              let safety = 10000 // 긴 악보를 위해 상향조정
              while (!cursor.Iterator.EndReached && safety > 0) {
                const ts = cursor.Iterator.currentTimeStamp?.RealValue || 0
                const measure = cursor.Iterator.CurrentMeasure

                // 마디 번호 대신 객체 참조를 사용하여 새로운 마디 진입을 정확히 감지
                if (measure && measure !== lastMeasureObj) {
                  starts.push(ts)
                  lastMeasureObj = measure
                }

                if (ts > lastValue) lastValue = ts
                cursor.next()
                safety--
              }

              cursor.reset()
              setMaxRealValue(lastValue)
              setMeasureStarts(starts)
            }
          } catch (e) {
            console.warn('[ScoreDisplay] Failed to calculate measures via cursor:', e)
          }

          // 렌더링 완료 후 약간의 대기
          await new Promise(resolve => setTimeout(resolve, 500))

          if (!isCancelled) {
            setCurrentPage(0)
            measureLayout()
          }
        }
      } catch (error) {
        if (!isCancelled) {
          console.error('[ScoreDisplay] Failed to load score:', error)
          if (onError && error instanceof Error) {
            onError(error)
          }
        }
      }
    }

    loadScore()

    return () => {
      isCancelled = true
    }
  }, [musicXmlUrl, onError, measureLayout])

  // 페이지 변경 시 SVG 위치 이동 (pageBreaks 기준). 한 페이지가 되면 이동을 되돌린다.
  useEffect(() => {
    if (!svgElement) return
    const offsetY = totalPages > 1 ? -(layout.breaks[currentPage] ?? 0) : 0

    svgElement.style.transform = offsetY === 0 ? '' : `translateY(${offsetY}px)`
    svgElement.style.transition = 'transform 0.3s ease'
  }, [currentPage, totalPages, svgElement, layout])

  // 진행률에 따른 커서 이동 — 루프는 악보(마디 목록)가 바뀔 때만 새로 만든다.
  // 페이지·분할·SVG는 ref로 읽어 페이지가 넘어가거나 리사이즈돼도 루프를 다시 만들지 않는다(P3-13).
  useEffect(() => {
    if (!osmdRef.current || !osmdRef.current.Sheet || maxRealValue === 0) return

    const osmd = osmdRef.current
    let animationFrameId: number
    let warned = false // 실패 로그는 루프당 한 번만(매 프레임 반복 방지)
    const warnOnce = (msg: string, e: unknown) => {
      if (warned) return
      warned = true
      console.warn(msg, e)
    }

    const updateCursor = () => {
      // 다음 프레임을 먼저 예약 — 커서가 아직 준비 안 돼 일찍 돌아가도 루프가 끊기지 않는다
      // (전에는 effect가 마디마다 다시 만들어져 우연히 되살아났다)
      animationFrameId = requestAnimationFrame(updateCursor)
      try {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any -- OSMD 커서 내부 API는 타입 미제공
        const cursor = osmd.cursor as any
        if (!cursor || !cursor.Iterator || measureStarts.length === 0) return

        // 커서 설정 보장 (박스 형태를 위해 선형 타입 사용)
        if (cursor.cursorOptions) {
          cursor.cursorOptions.color = '#FFFF0088' // 약간 투명한 노란색 (배경이 보이게)
          cursor.cursorOptions.type = 0 // 0: Normal (세로선)
          cursor.cursorOptions.followCursor = true
        }

        // 1. 목표 마디 인덱스 결정 (플레이어의 마디 번호를 물리적 순서로 취급)
        const targetIndex = clampMeasureIndex(currentMeasureRef.current, measureStarts.length)

        const targetTime = measureStarts[targetIndex]
        const currentTs = cursor.Iterator.currentTimeStamp?.RealValue || 0

        // 2. 방향 결정 및 이동 (목표 시간으로 강제 스냅)
        if (Math.abs(targetTime - currentTs) > 0.01) {
          // 뒤로 가야 하거나 차이가 큰 경우 초기화
          if (targetTime < currentTs || Math.abs(targetTime - currentTs) > 2.0) {
            cursor.reset()
          }

          // 타겟 시간을 향해 전진
          let safety = 5000
          while (!cursor.Iterator.EndReached && (cursor.Iterator.currentTimeStamp?.RealValue || 0) < targetTime && safety > 0) {
            cursor.next()
            safety--
          }
        }

        // 3. 커서 표시 및 마디 너비 확장
        cursor.show()

        try {
          const mObj = cursor.Iterator.CurrentMeasure
          // OSMD 커서는 컨테이너에 absolute로 붙는 <img>(SVG 요소 아님) — top/left/z-index는 OSMD가 넣는다
          const cursorElement = cursor.cursorElement as HTMLImageElement | undefined

          if (mObj && cursorElement) {
            // 보이기만 강제한다. style 전체를 덮어쓰면 OSMD의 position·top·left·z-index가 지워진다(M2)
            cursorElement.style.setProperty('display', 'block', 'important')
            cursorElement.style.setProperty('opacity', '1', 'important')
            cursorElement.style.setProperty('visibility', 'visible', 'important')
            // 커서는 SVG 밖이라 페이지 이동(translateY)을 따로 따라가야 악보와 맞는다
            const svgTransform = svgElementRef.current?.style.transform ?? ''
            if (cursorElement.style.transform !== svgTransform) {
              cursorElement.style.transform = svgTransform
              cursorElement.style.transition = 'transform 0.3s ease'
            }

            // 너비 계산 (GraphicalMeasures 우선)
            // eslint-disable-next-line @typescript-eslint/no-explicit-any -- OSMD 커서 내부 API는 타입 미제공
            const gMeasure = (mObj as any).GraphicalMeasures?.[0]
            let measureWidth = 0

            if (gMeasure && gMeasure.PositionAndShape) {
              measureWidth = gMeasure.PositionAndShape.Size.width * 10 * osmd.zoom
            } else {
              measureWidth = mObj.PositionAndShape.Size.width * 10 * osmd.zoom
            }

            if (measureWidth > 0) {
              cursorElement.setAttribute('width', measureWidth.toString())
            }
          }
        } catch (e) {
          warnOnce('[ScoreDisplay] Box styling failed:', e)
        }

        // 4. 페이지 자동 전환 제어
        const measureObj = cursor.Iterator.CurrentMeasure
        if (measureObj) {
          const mIdx = measureObj.MeasureNumber || measureObj.measureNumberInternal || 0

          // 마디 번호가 바뀌었을 때(또는 분할이 바뀐 직후)만 로직 수행
          if (mIdx !== lastMeasureIndexRef.current) {
            lastMeasureIndexRef.current = mIdx

            // 현재 커서의 수직 위치(y)를 확인하여 페이지 전환 여부 결정
            try {
              const cursorElement = cursor.cursorElement as HTMLImageElement | undefined
              if (cursorElement && svgElementRef.current) {
                // <img>라 getBBox가 없다(M1) — OSMD가 넣는 style.top(CSS px, SVG 위끝 기준, translate 전)을 쓴다.
                // breaks도 CSS px라 같은 단위
                const top = parseFloat(cursorElement.style.top)
                const cursorY = Number.isFinite(top) ? top : cursorElement.offsetTop

                // 어느 페이지에 속하는지 계산 (민감도 보정: 마진 50 → 더 빨리 인식)
                const targetPage = pageForY(pageBreaksRef.current, cursorY, 50)

                if (targetPage !== currentPageRef.current) {
                  currentPageRef.current = targetPage
                  setCurrentPage(targetPage)
                }
              }
            } catch (e) {
              warnOnce('[ScoreDisplay] Auto page switch failed:', e)
            }
          }
        }

      } catch (e) {
        warnOnce('[ScoreDisplay] Cursor update failed:', e)
      }
    }

    animationFrameId = requestAnimationFrame(updateCursor)

    return () => {
      cancelAnimationFrame(animationFrameId)
    }
  }, [maxRealValue, measureStarts])

  // 리사이즈 시 페이지 재계산 — 첫 렌더와 같은 measureLayout. OSMD autoResize 재렌더가 끝난 뒤 한 번만 잰다.
  // window resize와 함께 컨테이너 너비 변화(사이드 패널 등)도 본다. 높이는 우리가 pageHeight로 바꾸므로 무시.
  // 악보가 그려진 뒤(svgElement)에만 구독하고, 측정이 SVG를 잠깐 못 찾아도 svgElement를 비우지 않아 구독이 유지된다.
  useEffect(() => {
    const container = containerRef.current
    if (!svgElement || !container) return

    let timer: ReturnType<typeof setTimeout> | undefined
    const schedule = () => {
      if (timer !== undefined) clearTimeout(timer)
      timer = setTimeout(measureLayout, RESIZE_SETTLE_MS)
    }

    let lastWidth = container.clientWidth
    const observer = typeof ResizeObserver === 'undefined'
      ? null
      : new ResizeObserver(() => {
        const width = container.clientWidth
        if (width === lastWidth) return
        lastWidth = width
        schedule()
      })
    observer?.observe(container)
    window.addEventListener('resize', schedule)
    return () => {
      observer?.disconnect()
      window.removeEventListener('resize', schedule)
      if (timer !== undefined) clearTimeout(timer)
    }
  }, [svgElement, measureLayout])

  const handlePrevPage = () => {
    if (currentPage > 0) {
      setCurrentPage(currentPage - 1)
    }
  }

  const handleNextPage = () => {
    if (currentPage < totalPages - 1) {
      setCurrentPage(currentPage + 1)
    }
  }

  return (
    <div style={{ position: 'relative', width: '100%', height: '100%' }}>
      {/* 페이지 네비게이션 */}
      {totalPages > 1 && (
        <div
          style={{
            position: 'sticky',
            top: '10px',
            right: '10px',
            zIndex: 10,
            display: 'flex',
            justifyContent: 'flex-end',
            marginBottom: '10px',
          }}
        >
          <div
            style={{
              display: 'flex',
              gap: '10px',
              alignItems: 'center',
              background: 'var(--white-a95)',
              padding: '10px var(--space-4)',
              borderRadius: 'var(--radius-md)',
              boxShadow: '0 2px 12px var(--black-a15)',
              border: '1px solid var(--neutral-250)',
            }}
          >
            <button
              onClick={handlePrevPage}
              disabled={currentPage === 0}
              style={{
                padding: 'var(--space-2) var(--space-4)',
                border: 'none',
                borderRadius: 'var(--radius-sm)',
                background: currentPage === 0 ? 'var(--neutral-200)' : 'var(--blue-500)',
                color: currentPage === 0 ? 'var(--neutral-450)' : 'var(--white)',
                cursor: currentPage === 0 ? 'not-allowed' : 'pointer',
                fontSize: '14px',
                fontWeight: '500',
                transition: 'all 0.2s',
              }}
            >
              ◀ 이전
            </button>
            <span style={{
              fontSize: '15px',
              fontWeight: '600',
              minWidth: '60px',
              textAlign: 'center',
            }}>
              {currentPage + 1} / {totalPages}
            </span>
            <button
              onClick={handleNextPage}
              disabled={currentPage === totalPages - 1}
              style={{
                padding: 'var(--space-2) var(--space-4)',
                border: 'none',
                borderRadius: 'var(--radius-sm)',
                background: currentPage === totalPages - 1 ? 'var(--neutral-200)' : 'var(--blue-500)',
                color: currentPage === totalPages - 1 ? 'var(--neutral-450)' : 'var(--white)',
                cursor: currentPage === totalPages - 1 ? 'not-allowed' : 'pointer',
                fontSize: '14px',
                fontWeight: '500',
                transition: 'all 0.2s',
              }}
            >
              다음 ▶
            </button>
          </div>
        </div>
      )}

      {/* 악보 컨테이너 */}
      <div
        ref={containerRef}
        className="score-display-container"
        style={{
          width: '100%',
          minHeight: totalPages > 1 ? `${pageHeight}px` : 'auto',
          maxHeight: totalPages > 1 ? `${pageHeight}px` : 'none',
          overflow: totalPages > 1 ? 'hidden' : 'auto',
          background: 'var(--white)',
          borderRadius: 'var(--radius-md)',
          padding: '1rem',
          position: 'relative',
        }}
      />
    </div>
  )
}
