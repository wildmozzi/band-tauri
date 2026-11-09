import { Midi } from '@tonejs/midi'

/**
 * MusicXML을 MIDI로 변환
 */
export async function convertMusicXmlToMidi(xmlUrl: string): Promise<Midi> {
  // XML 파일 로드
  const response = await fetch(xmlUrl)
  const xmlText = await response.text()
  
  // XML 파싱
  const parser = new DOMParser()
  const xmlDoc = parser.parseFromString(xmlText, 'text/xml')
  
  // MIDI 객체 생성
  const midi = new Midi()
  const track = midi.addTrack()
  
  // 기본 설정
  let divisions = 1  // MusicXML divisions per quarter note
  let tempo = 120    // BPM
  let currentTime = 0  // 현재 시간 (초)
  
  // tempo 찾기
  const soundElements = xmlDoc.querySelectorAll('sound[tempo]')
  if (soundElements.length > 0) {
    tempo = parseFloat(soundElements[0].getAttribute('tempo') || '120')
  }
  
  // direction에서 tempo 찾기
  const directionTempos = xmlDoc.querySelectorAll('direction sound[tempo]')
  if (directionTempos.length > 0) {
    tempo = parseFloat(directionTempos[0].getAttribute('tempo') || '120')
  }
  
  midi.header.setTempo(tempo)
  
  // 초당 박자 수 계산
  const quarterNoteDuration = 60 / tempo  // 4분음표 길이 (초)
  
  // 모든 part 처리
  const parts = xmlDoc.querySelectorAll('part')
  
  parts.forEach(part => {
    currentTime = 0
    
    // 모든 measure 처리
    const measures = part.querySelectorAll('measure')
    
    measures.forEach(measure => {
      // attributes에서 divisions 가져오기
      const attributesDiv = measure.querySelector('attributes divisions')
      if (attributesDiv) {
        divisions = parseInt(attributesDiv.textContent || '1')
      }
      
      // measure 내의 모든 요소 순회
      const children = measure.children
      
      for (let i = 0; i < children.length; i++) {
        const element = children[i]
        
        if (element.tagName === 'note') {
          const isRest = element.querySelector('rest') !== null
          const isChord = element.querySelector('chord') !== null
          
          // duration 가져오기
          const durationEl = element.querySelector('duration')
          const duration = durationEl ? parseInt(durationEl.textContent || '0') : 0
          const durationInSeconds = (duration / divisions) * quarterNoteDuration
          
          if (!isRest) {
            // pitch 정보 가져오기
            const pitchEl = element.querySelector('pitch')
            if (pitchEl) {
              const step = pitchEl.querySelector('step')?.textContent || 'C'
              const octave = parseInt(pitchEl.querySelector('octave')?.textContent || '4')
              const alter = parseInt(pitchEl.querySelector('alter')?.textContent || '0')
              
              const midiPitch = stepToMidi(step, octave, alter)
              
              // velocity (dynamics)
              const dynamicsEl = element.querySelector('dynamics')
              let velocity = 80  // 기본값
              if (dynamicsEl) {
                const dynamicType = dynamicsEl.children[0]?.tagName
                velocity = dynamicToVelocity(dynamicType)
              }
              
              // 코드가 아니면 시간 업데이트는 나중에
              const noteStartTime = isChord ? currentTime - durationInSeconds : currentTime
              
              // 노트 추가
              track.addNote({
                midi: midiPitch,
                time: Math.max(0, noteStartTime),
                duration: durationInSeconds,
                velocity: velocity / 127
              })
            }
          }
          
          // 코드가 아닌 경우에만 시간 진행
          if (!isChord) {
            currentTime += durationInSeconds
          }
        }
        
        // forward/backup 처리
        if (element.tagName === 'forward') {
          const dur = parseInt(element.querySelector('duration')?.textContent || '0')
          currentTime += (dur / divisions) * quarterNoteDuration
        }
        
        if (element.tagName === 'backup') {
          const dur = parseInt(element.querySelector('duration')?.textContent || '0')
          currentTime -= (dur / divisions) * quarterNoteDuration
          currentTime = Math.max(0, currentTime)
        }
      }
    })
  })
  
  console.log(`[MusicXmlToMidi] Converted: ${track.notes.length} notes, tempo: ${tempo} BPM`)
  
  return midi
}

/**
 * 음이름을 MIDI 번호로 변환
 */
function stepToMidi(step: string, octave: number, alter: number): number {
  const stepMap: Record<string, number> = {
    'C': 0, 'D': 2, 'E': 4, 'F': 5, 'G': 7, 'A': 9, 'B': 11
  }
  
  const baseNote = stepMap[step.toUpperCase()] || 0
  const midiNote = (octave + 1) * 12 + baseNote + alter
  
  return Math.max(0, Math.min(127, midiNote))
}

/**
 * 다이나믹을 velocity로 변환
 */
function dynamicToVelocity(dynamic?: string): number {
  const dynamicMap: Record<string, number> = {
    'ppp': 20,
    'pp': 35,
    'p': 50,
    'mp': 65,
    'mf': 80,
    'f': 95,
    'ff': 110,
    'fff': 125
  }
  
  return dynamicMap[dynamic || 'mf'] || 80
}

/**
 * Midi 객체를 Blob URL로 변환
 */
export function midiToUrl(midi: Midi): string {
  const midiArray = midi.toArray()
  const blob = new Blob([new Uint8Array(midiArray).buffer], { type: 'audio/midi' })
  return URL.createObjectURL(blob)
}
