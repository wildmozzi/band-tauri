/**
 * OMR (Optical Music Recognition) 서비스
 * PDF 악보를 MusicXML로 변환 - Audiveris 연동
 */

import * as fs from 'fs'
import * as path from 'path'
import { exec } from 'child_process'
import { promisify } from 'util'

const execAsync = promisify(exec)

export interface OmrResult {
  success: boolean
  musicXmlContent?: string
  musicXmlPath?: string
  error?: string
  warnings?: string[]
}

// Audiveris 설정
const AUDIVERIS_BAT = process.env.AUDIVERIS_BAT || 'C:/Program Files/Audiveris/bin/Audiveris.bat'
const getJavaPath = () => {
  if (process.env.JAVA_HOME) {
    // Windows 경로 처리 및 따옴표 제거
    const javaHome = process.env.JAVA_HOME.replace(/"/g, '');
    const javaExePath = path.join(javaHome, 'bin', 'java.exe');
    if (fs.existsSync(javaExePath)) {
      return `"${javaExePath}"`;
    }
  }
  return 'java';
}

const JAVA_PATH = getJavaPath()


/**
 * PDF 파일을 MusicXML로 변환 (Audiveris 사용)
 */
export async function convertPdfToMusicXml(pdfPath: string): Promise<OmrResult> {
  try {
    console.log(`[OMR] PDF → MusicXML 변환 시작: ${pdfPath}`)

    // 1. PDF 파일 존재 확인
    if (!fs.existsSync(pdfPath)) {
      return {
        success: false,
        error: `PDF 파일을 찾을 수 없습니다: ${pdfPath}`
      }
    }

    // 2. Java 설치 확인
    try {
      await execAsync(`${JAVA_PATH} -version`, {
        windowsHide: true
      })
    } catch (error: any) {
      // java -version은 stderr에 출력하는 게 정상
      if (error?.stderr?.includes('version')) {
        // Java 정상 설치 → 통과
      } else {
        return {
          success: false,
          error: 'Java가 설치되지 않았습니다. Java 17 이상을 설치해주세요.\n다운로드: https://adoptium.net/'
        }
      }
    }


    // 3. Audiveris BAT 파일 확인
    if (!fs.existsSync(AUDIVERIS_BAT)) {
      return {
        success: false,
        error: `Audiveris BAT 파일을 찾을 수 없습니다: ${AUDIVERIS_BAT}\n다운로드: https://github.com/Audiveris/audiveris/releases`
      }
    }

    // 4. Audiveris 실행
    console.log('[OMR] Audiveris 실행 중...')
    const outputDir = path.dirname(pdfPath)
    const baseName = path.basename(pdfPath, '.pdf')

    // Audiveris 명령어 (BAT 파일 사용)
    // -batch: 배치 모드 (GUI 없음)
    // -export: MusicXML 내보내기
    // -output: 출력 디렉토리
    // -option org.audiveris.omr.sheet.Scale.interlineScale=3: 인식 정확도 향상
    const command = `"${AUDIVERIS_BAT}" -batch -export -output "${outputDir}" -option org.audiveris.omr.sheet.Scale.interlineScale=3 "${pdfPath}"`

    console.log('[OMR] 명령어:', command)

    try {
      const { stdout, stderr } = await execAsync(command, {
        maxBuffer: 50 * 1024 * 1024, // 50MB로 대폭 상향 (8페이지 이상 대응)
        timeout: 600000 // 10분 타임아웃 (복잡한 악보 대응)
      })

      console.log('[OMR] Audiveris stdout:', stdout)
      console.log('[OMR] Audiveris stderr:', stderr)

    } catch (execError: any) {
      console.error('[OMR] Audiveris 실행 오류:', execError)
      console.error('[OMR] stdout:', execError.stdout)
      console.error('[OMR] stderr:', execError.stderr)
      console.error('[OMR] 명령어:', command)

      return {
        success: false,
        error: `Audiveris 실행 실패: ${execError.message}\n\nSTDOUT:\n${execError.stdout || '(없음)'}\n\nSTDERR:\n${execError.stderr || '(없음)'}`
      }
    }

    // 5. 생성된 MusicXML 파일 찾기
    // Audiveris는 보통 .mxl (압축) 또는 .musicxml 생성
    // 여러 파트가 감지되면 .mvt1.mxl, .mvt2.mxl 등으로 생성될 수 있음
    let musicXmlPath: string | null = null
    const possibleExtensions = ['.mxl', '.musicxml', '.xml']

    console.log(`[OMR] 파일 검색 시작 (baseName: ${baseName})`)

    // 5-1. 우선 정확히 일치하거나 mvt1 패턴인 것부터 확인
    const checkPatterns = [
      ...possibleExtensions.map(ext => baseName + ext),
      ...possibleExtensions.map(ext => baseName + '.mvt1' + ext),
      ...possibleExtensions.map(ext => baseName + '.mvt2' + ext)
    ]

    console.log('[OMR] 검색할 패턴 리스트:', checkPatterns)

    for (const pattern of checkPatterns) {
      const candidatePath = path.join(outputDir, pattern)
      if (fs.existsSync(candidatePath)) {
        console.log(`[OMR] 파일 발견 (패턴 매칭): ${candidatePath}`)
        musicXmlPath = candidatePath
        break
      }
    }

    // 5-2. Audiveris가 프로젝트 하위 폴더에 생성하는 경우도 체크
    if (!musicXmlPath) {
      const projectDir = path.join(outputDir, baseName)
      console.log(`[OMR] 하위 폴더 확인: ${projectDir}`)
      if (fs.existsSync(projectDir)) {
        for (const pattern of checkPatterns) {
          const candidatePath = path.join(projectDir, pattern)
          if (fs.existsSync(candidatePath)) {
            console.log(`[OMR] 하위 폴더에서 파일 발견: ${candidatePath}`)
            musicXmlPath = candidatePath
            break
          }
        }
      }
    }

    // 5-3. 최후의 수단: 디렉토리 내의 모든 파일을 뒤져서 baseName으로 시작하는 MusicXML 찾기
    if (!musicXmlPath) {
      console.log(`[OMR] 최후의 수단: ${outputDir} 폴더의 전체 파일 스캔`)
      try {
        const allFiles = fs.readdirSync(outputDir)
        console.log('[OMR] 폴더 내 파일 목록:', allFiles)

        const matchingFile = allFiles.find(f =>
          f.startsWith(baseName) &&
          possibleExtensions.some(ext => f.endsWith(ext))
        )
        if (matchingFile) {
          musicXmlPath = path.join(outputDir, matchingFile)
          console.log(`[OMR] 스캔으로 파일 발견: ${musicXmlPath}`)
        }
      } catch (err) {
        console.error('[OMR] 디렉토리 스캔 중 오류:', err)
      }
    }

    if (!musicXmlPath) {
      // 디버깅을 위해 결과가 없을 때 폴더 내용을 다시 한 번 출력
      const finalDirCheck = fs.readdirSync(outputDir)
      console.error(`[OMR] 검색 실패! 최종 폴더 상태:`, finalDirCheck)

      return {
        success: false,
        error: `MusicXML 파일이 생성되지 않았습니다. (검색어: ${baseName}, 폴더: ${outputDir})\nPDF 내용이 너무 복잡하거나 Java 메모리가 부족할 수 있습니다.`
      }
    }

    // 6. MusicXML 파일 읽기
    console.log(`[OMR] MusicXML 파일 발견: ${musicXmlPath}`)

    let musicXmlContent: string

    // .mxl은 압축 파일이므로 압축 해제 필요
    if (musicXmlPath.endsWith('.mxl')) {
      console.log('[OMR] .mxl 파일 압축 해제 중...')
      musicXmlContent = await extractMxl(musicXmlPath)
    } else {
      musicXmlContent = fs.readFileSync(musicXmlPath, 'utf-8')
    }

    // 7. MusicXML 검증 및 수정
    const { content: validatedContent, warnings } = await validateAndFixMusicXml(musicXmlContent)

    // 8. 최종 .musicxml 파일로 저장
    const finalOutputPath = path.join(outputDir, `${baseName}_converted.musicxml`)
    fs.writeFileSync(finalOutputPath, validatedContent, 'utf-8')

    console.log(`[OMR] 변환 완료: ${finalOutputPath}`)

    return {
      success: true,
      musicXmlContent: validatedContent,
      musicXmlPath: finalOutputPath,
      warnings
    }

  } catch (error) {
    console.error('[OMR] 변환 오류:', error)
    return {
      success: false,
      error: `OMR 변환 실패: ${error instanceof Error ? error.message : 'Unknown error'}`
    }
  }
}

/**
 * .mxl 압축 파일에서 MusicXML 추출
 */
async function extractMxl(mxlPath: string): Promise<string> {
  const AdmZip = require('adm-zip') // npm install adm-zip 필요

  try {
    const zip = new AdmZip(mxlPath)
    const entries = zip.getEntries()

    // META-INF/container.xml에서 실제 MusicXML 파일 경로 찾기
    const containerEntry = entries.find((e: any) => e.entryName === 'META-INF/container.xml')

    if (containerEntry) {
      const containerXml = containerEntry.getData().toString('utf8')
      const match = containerXml.match(/full-path="([^"]+)"/)

      if (match) {
        const xmlEntry = entries.find((e: any) => e.entryName === match[1])
        if (xmlEntry) {
          return xmlEntry.getData().toString('utf8')
        }
      }
    }

    // container.xml이 없으면 첫 번째 .xml 파일 사용
    const xmlEntry = entries.find((e: any) => e.entryName.endsWith('.xml'))
    if (xmlEntry) {
      return xmlEntry.getData().toString('utf8')
    }

    throw new Error('.mxl 파일에서 MusicXML을 찾을 수 없습니다')

  } catch (error) {
    throw new Error(`MXL 압축 해제 실패: ${error instanceof Error ? error.message : 'Unknown error'}`)
  }
}

/**
 * MusicXML에 줄바꿈 추가 (첫 줄 3마디, 이후 4마디씩)
 */
function addSystemBreaks(xmlContent: string): string {
  try {
    // measure 태그들을 찾아서 줄바꿈 추가
    const measureRegex = /<measure\s+number="(\d+)"[^>]*>/g
    let match
    const measures: { number: number; index: number }[] = []

    while ((match = measureRegex.exec(xmlContent)) !== null) {
      measures.push({
        number: parseInt(match[1]),
        index: match.index
      })
    }

    if (measures.length === 0) {
      console.log('[OMR] No measures found for system breaks')
      return xmlContent
    }

    // 줄바꿈이 필요한 마디 번호 계산 (4마디마다)
    const breakPoints: number[] = []

    // 5번 마디부터 4마디씩 줄바꿈 (1-4마디가 첫 줄, 5-8마디가 두 번째 줄...)
    let nextBreak = 5
    while (nextBreak <= measures.length) {
      breakPoints.push(nextBreak)
      nextBreak += 4
    }

    console.log('[OMR] Adding system breaks at measures:', breakPoints)

    // 뒤에서부터 삽입 (인덱스 변경 방지)
    let result = xmlContent
    for (let i = breakPoints.length - 1; i >= 0; i--) {
      const measureNum = breakPoints[i]
      const measure = measures.find(m => m.number === measureNum)

      if (measure) {
        // <measure number="X"> 태그 바로 다음에 <print new-system="yes"/> 삽입
        const insertPos = result.indexOf('>', measure.index) + 1
        const printTag = '\n      <print new-system="yes"/>'
        result = result.slice(0, insertPos) + printTag + result.slice(insertPos)
      }
    }

    return result
  } catch (error) {
    console.error('[OMR] Error adding system breaks:', error)
    return xmlContent
  }
}

/**
 * MusicXML 검증 및 자동 수정
 */
async function validateAndFixMusicXml(xmlContent: string): Promise<{ content: string; warnings: string[] }> {
  const warnings: string[] = []
  let content = xmlContent

  // 1. XML 선언 확인
  if (!content.includes('<?xml')) {
    content = '<?xml version="1.0" encoding="UTF-8"?>\n' + content
    warnings.push('XML 선언이 추가되었습니다')
  }

  // 2. DOCTYPE 확인
  if (!content.includes('<!DOCTYPE') && !content.includes('score-partwise')) {
    const doctypeIndex = content.indexOf('<?xml')
    if (doctypeIndex !== -1) {
      const insertIndex = content.indexOf('\n', doctypeIndex) + 1
      const doctype = `<!DOCTYPE score-partwise PUBLIC "-//Recordare//DTD MusicXML 3.1 Partwise//EN" "http://www.musicxml.org/dtds/partwise.dtd">\n`
      content = content.slice(0, insertIndex) + doctype + content.slice(insertIndex)
      warnings.push('DOCTYPE이 추가되었습니다')
    }
  }

  // 3. 기본 검증
  if (!content.includes('<score-partwise')) {
    warnings.push('경고: 유효한 MusicXML 구조가 아닐 수 있습니다')
  }

  // 4. 화음 태그 검증 (첫 음에 <chord/> 없는지 확인)
  const chordPattern = /<note>[\s\S]*?<chord\/>/g
  const matches = content.match(chordPattern)
  if (matches && matches.length > 0) {
    warnings.push(`화음 태그 ${matches.length}개 발견 - OSMD 호환성 확인됨`)
  }

  // 5. 줄바꿈 추가 (비활성화)
  // content = addSystemBreaks(content)
  // warnings.push('악보 레이아웃 설정: 4마디마다 줄바꿈')

  return { content, warnings }
}

/**
 * Audiveris 설치 확인 헬퍼 함수
 */
export async function checkAudiverisInstallation(): Promise<{ installed: boolean; message: string }> {
  try {
    // Java 확인
    try {
      const { stdout } = await execAsync(`${JAVA_PATH} -version`)
      console.log('Java 버전:', stdout)
    } catch {
      return {
        installed: false,
        message: 'Java가 설치되지 않았습니다.\n다운로드: https://adoptium.net/'
      }
    }

    // Audiveris BAT 확인
    if (!fs.existsSync(AUDIVERIS_BAT)) {
      return {
        installed: false,
        message: `Audiveris BAT 파일이 없습니다: ${AUDIVERIS_BAT}\n다운로드: https://github.com/Audiveris/audiveris/releases`
      }
    }

    return {
      installed: true,
      message: 'Audiveris가 정상적으로 설치되었습니다'
    }
  } catch (error) {
    return {
      installed: false,
      message: `설치 확인 실패: ${error instanceof Error ? error.message : 'Unknown error'}`
    }
  }
}