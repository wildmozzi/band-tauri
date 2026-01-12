export type RoomStatus = 'open' | 'recording' | 'locked'

// 합주실 타입 (간소화됨 - DB 스키마와 일치)
export type Room = {
  id: string
  title: string
  genre: string
  vibe: string
  capacity: number
  musicians: number
  status: RoomStatus
}

export type RoomFilter = {
  id: string
  label: string
  match?: (room: Room) => boolean
}

// 합주실 장르 필터 (CreateRoom.tsx의 GENRES와 일치)
export const ROOM_FILTERS: RoomFilter[] = [
  { id: 'all', label: '전체' },
  { id: 'rock', label: '록', match: (room) => room.genre === '록' },
  { id: 'jazz', label: '재즈', match: (room) => room.genre === '재즈' },
  { id: 'blues', label: '블루스', match: (room) => room.genre === '블루스' },
  { id: 'classic', label: '클래식', match: (room) => room.genre === '클래식' },
  { id: 'pop', label: '팝', match: (room) => room.genre === '팝' },
  { id: 'hiphop', label: '힙합', match: (room) => room.genre === '힙합' },
  { id: 'electronic', label: '일렉트로닉', match: (room) => room.genre === '일렉트로닉' },
  { id: 'folk', label: '포크', match: (room) => room.genre === '포크' },
  { id: 'metal', label: '메탈', match: (room) => room.genre === '메탈' },
  { id: 'punk', label: '펑크', match: (room) => room.genre === '펑크' },
  { id: 'reggae', label: '레게', match: (room) => room.genre === '레게' },
  { id: 'other', label: '기타', match: (room) => room.genre === '기타' },
]
