import { useEffect, useRef } from 'react'
import { OpenSheetMusicDisplay } from 'opensheetmusicdisplay'

interface ScoreDisplayProps {
  musicXmlUrl?: string
  onError?: (error: Error) => void
  currentProgress?: number
}

/**
 * OpenSheetMusicDisplay를 사용한 악보 시각화 컴포넌트
 */
export function ScoreDisplay({ musicXmlUrl, onError, currentProgress = 0 }: ScoreDisplayProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const osmdRef = useRef<OpenSheetMusicDisplay | null>(null)

  useEffect(() => {
    if (!containerRef.current) return

    // OpenSheetMusicDisplay 초기화
    const osmd = new OpenSheetMusicDisplay(containerRef.current, {
      autoResize: true,
      backend: 'svg',
      drawTitle: true,
      drawingParameters: 'compact',
    })

    osmdRef.current = osmd

    return () => {
      // 정리
      osmdRef.current = null
    }
  }, [])

  useEffect(() => {
    if (!osmdRef.current || !musicXmlUrl) return

    const loadScore = async () => {
      try {
        console.log('[ScoreDisplay] Loading MusicXML:', musicXmlUrl)

        // MusicXML 로드
        await osmdRef.current!.load(musicXmlUrl)

        // 악보 렌더링
        osmdRef.current!.render()

        console.log('[ScoreDisplay] Score rendered successfully')
      } catch (error) {
        console.error('[ScoreDisplay] Failed to load score:', error)
        if (onError && error instanceof Error) {
          onError(error)
        }
      }
    }

    loadScore()
  }, [musicXmlUrl, onError])

  // 재생 진행에 따라 커서 이동 (선택적 기능)
  useEffect(() => {
    if (!osmdRef.current || currentProgress === 0) return

    try {
      // OSMD의 커서 기능 사용
      // 참고: 이 기능은 OSMD 버전에 따라 다를 수 있습니다
      // const cursor = osmdRef.current.cursor
      // if (cursor) {
      //   cursor.show()
      //   // 진행률에 따라 커서 위치 업데이트
      // }
    } catch (error) {
      console.error('[ScoreDisplay] Cursor update error:', error)
    }
  }, [currentProgress])

  return (
    <div
      ref={containerRef}
      className="score-display-container"
      style={{
        width: '100%',
        height: '100%',
        overflow: 'auto',
        background: 'white',
        borderRadius: '8px',
        padding: '1rem'
      }}
    />
  )
}
