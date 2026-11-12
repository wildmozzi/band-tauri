import { useEffect, useRef, useState } from 'react'
import { OpenSheetMusicDisplay } from 'opensheetmusicdisplay'
import {
  a4HeightFor,
  clampMeasureIndex,
  fallbackPageCount,
  findScoreSystems,
  pageBreaksByBottom,
  pageBreaksByHeight,
  pageForY,
  systemBoxes,
} from './scoreLayout'

interface ScoreDisplayProps {
  musicXmlUrl?: string
  currentProgress?: number // 0 ~ 1 사이의 진행률
  currentMeasure?: number // [추가] 현재 마디 번호 (0부터 시작)
  onError?: (error: Error) => void
}

/**
 * OpenSheetMusicDisplay를 사용한 악보 시각화 컴포넌트
 * 페이지 단위로 표시
 */
export function ScoreDisplay({ musicXmlUrl, currentProgress = 0, currentMeasure = 0, onError }: ScoreDisplayProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const osmdRef = useRef<OpenSheetMusicDisplay | null>(null)
  const [currentPage, setCurrentPage] = useState(0)
  const [totalPages, setTotalPages] = useState(0)
  const [svgElement, setSvgElement] = useState<SVGElement | null>(null)
  const [pageHeight, setPageHeight] = useState(1100)
  const [pageBreaks, setPageBreaks] = useState<number[]>([0])
  const [maxRealValue, setMaxRealValue] = useState(0)
  const [measureStarts, setMeasureStarts] = useState<number[]>([])

  // 하이라이트 점프 및 깜빡임 방지를 위한 Ref
  const lastTargetTimeRef = useRef(0)
  const currentProgressRef = useRef(0)
  const currentMeasureRef = useRef(0)
  const lastMeasureIndexRef = useRef(-1)

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
        } catch (e) {
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
        console.log('[ScoreDisplay] Loading MusicXML:', musicXmlUrl)
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
            // eslint-disable-next-line @typescript-eslint/no-explicit-any -- OSMD 커서 내부 API는 타입 미제공
            const cursor = osmd.cursor as any
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
              console.log(`[ScoreDisplay] Cursor-based calc done. Measures: ${starts.length}, Max: ${lastValue.toFixed(2)}`)
            }
          } catch (e) {
            console.warn('[ScoreDisplay] Failed to calculate measures via cursor:', e)
          }

          console.log('[ScoreDisplay] Render completed, setting timeout...')

          // 렌더링 완료 후 약간의 대기
          await new Promise(resolve => setTimeout(resolve, 500))

          console.log('[ScoreDisplay] Starting page calculation, isCancelled:', isCancelled)

          if (!isCancelled && containerRef.current) {
            const svg = containerRef.current.querySelector('svg')
            console.log('[ScoreDisplay] SVG found:', !!svg)

            if (svg) {
              setSvgElement(svg as SVGElement)

              // System(악보 줄) 요소들 찾기 - 여러 가능한 셀렉터 시도
              const systems = findScoreSystems(svg, (label, count) =>
                console.log(`[ScoreDisplay] Found systems (${label}):`, count))

              if (systems.length > 0) {
                // SVG의 실제 크기 가져오기
                const svgBBox = svg.getBBox()
                const svgWidth = svgBBox.width
                const svgHeight = svgBBox.height

                // A4 비율: 210mm x 297mm = 1:1.414
                // SVG 너비를 기준으로 A4 높이 계산
                const a4Height = a4HeightFor(svgWidth)

                console.log('[ScoreDisplay] SVG size:', svgWidth, 'x', svgHeight, 'A4 height:', a4Height)

                // 각 system의 y 위치를 기준으로 페이지 분할
                const { breaks: pageBreaks, lastBottom: lastSystemBottom } = pageBreaksByBottom(systemBoxes(systems), a4Height)

                const calculatedPages = pageBreaks.length
                setTotalPages(calculatedPages)
                setCurrentPage(0)
                setPageHeight(a4Height)
                setPageBreaks(pageBreaks)

                console.log('[ScoreDisplay] Pages:', calculatedPages, 'Breaks:', pageBreaks, 'Last system bottom:', lastSystemBottom)
              } else {
                // System을 못 찾으면 기존 방식 사용
                const svgHeight = svg.getBoundingClientRect().height
                const containerWidth = containerRef.current.clientWidth
                const a4Height = a4HeightFor(containerWidth)
                const calculatedPages = fallbackPageCount(svgHeight, a4Height)

                setTotalPages(calculatedPages)
                setCurrentPage(0)
                setPageHeight(a4Height)
                console.log('[ScoreDisplay] Fallback - SVG height:', svgHeight, 'Pages:', calculatedPages)
              }
            } else {
              setTotalPages(1)
              console.log('[ScoreDisplay] No SVG found, defaulting to 1 page')
            }
          } else {
            console.log('[ScoreDisplay] Cancelled or containerRef is null')
          }

          console.log('[ScoreDisplay] Score rendered successfully')
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
  }, [musicXmlUrl, onError])

  // 페이지 변경 시 SVG 위치 이동 (pageBreaks 기준)
  useEffect(() => {
    if (!svgElement || totalPages <= 1) return

    const offsetY = -pageBreaks[currentPage]

    svgElement.style.transform = `translateY(${offsetY}px)`
    svgElement.style.transition = 'transform 0.3s ease'

    console.log('[ScoreDisplay] Moving to page', currentPage + 1, 'offset:', offsetY)
  }, [currentPage, totalPages, svgElement, pageBreaks])

  // 진행률에 따른 커서 이동 (최적화된 방식)
  useEffect(() => {
    if (!osmdRef.current || !osmdRef.current.Sheet || maxRealValue === 0) return

    const osmd = osmdRef.current
    let animationFrameId: number

    const updateCursor = () => {
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
        const playerBarIndex = currentMeasureRef.current
        const targetIndex = clampMeasureIndex(playerBarIndex, measureStarts.length)

        const targetTime = measureStarts[targetIndex]
        const currentTs = cursor.Iterator.currentTimeStamp?.RealValue || 0

        // 디버깅 로그 (샘플링)
        if (Math.random() < 0.05) {
          console.log(`[ScoreDisplay] PDF Sync - BarIdx: ${playerBarIndex}, TargetTime: ${targetTime.toFixed(2)}, CurrentTime: ${currentTs.toFixed(2)}`)
        }

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
          const cursorElement = cursor.cursorElement as SVGRectElement

          if (mObj && cursorElement) {
            // 매우 강한 스타일 주입 (OSMD 내부 설정을 무시하도록 !important 사용)
            cursorElement.setAttribute('style', 'display: block !important; fill: rgba(255, 255, 0, 0.45) !important; stroke: none !important; opacity: 1 !important; visibility: visible !important;')

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
          if (Math.random() < 0.05) console.warn('[ScoreDisplay] Box styling failed:', e)
        }

        // 4. 페이지 자동 전환 제어
        const currentMeasure = cursor.Iterator.CurrentMeasure
        if (currentMeasure) {
          const mIdx = currentMeasure.MeasureNumber || currentMeasure.measureNumberInternal || 0

          // 마디 번호가 바뀌었을 때만 로직 수행
          if (mIdx !== lastMeasureIndexRef.current) {
            lastMeasureIndexRef.current = mIdx

            // 현재 커서의 수직 위치(y)를 확인하여 페이지 전환 여부 결정
            try {
              const cursorElement = cursor.cursorElement
              if (cursorElement && svgElement) {
                const cursorBBox = cursorElement.getBBox()
                const cursorY = cursorBBox.y

                // 어느 페이지에 속하는지 계산 (민감도 보정)
                const targetPage = pageForY(pageBreaks, cursorY, 50) // 10 -> 50으로 마진 확대 (더 빨리 인식)

                if (targetPage !== currentPage) {
                  console.log(`[ScoreDisplay] Auto switching to page ${targetPage + 1} (measure: ${mIdx})`)
                  setCurrentPage(targetPage)
                }
              }
            } catch (e) {
              // getBBox 실패 시 무시
            }
          }
        }

      } catch (e) {
        console.warn('[ScoreDisplay] Cursor update failed:', e)
      }

      animationFrameId = requestAnimationFrame(updateCursor)
    }

    animationFrameId = requestAnimationFrame(updateCursor)

    return () => {
      cancelAnimationFrame(animationFrameId)
    }
  }, [maxRealValue, measureStarts, svgElement, pageBreaks, currentPage, currentMeasure])

  // 화면 리사이즈 시 페이지 재계산
  useEffect(() => {
    if (!containerRef.current || !svgElement) return

    const handleResize = () => {
      // System(악보 줄) 요소들 찾기 - 여러 가능한 셀렉터 시도
      const systems = findScoreSystems(svgElement)

      if (systems.length > 0) {
        const containerWidth = containerRef.current!.clientWidth
        const a4Height = a4HeightFor(containerWidth)

        const pageBreaks = pageBreaksByHeight(systemBoxes(systems), a4Height)

        const calculatedPages = pageBreaks.length
        setPageHeight(a4Height)
        setTotalPages(calculatedPages)
        setPageBreaks(pageBreaks)

        if (currentPage >= calculatedPages) {
          setCurrentPage(calculatedPages - 1)
        }

        console.log('[ScoreDisplay] Resized - pages:', calculatedPages, 'breaks:', pageBreaks)
      }
    }

    window.addEventListener('resize', handleResize)
    return () => window.removeEventListener('resize', handleResize)
  }, [svgElement, currentPage])

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
