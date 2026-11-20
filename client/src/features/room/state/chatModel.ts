/**
 * 방 채팅 (순수 함수). 수신 처리는 signalReducer, 보내기는 RoomProvider 명령.
 */
export interface ChatMessage {
  id: string
  peerId: string
  nickname: string
  message: string
  timestamp: number
}

export const MAX_CHAT_MESSAGES = 500

let chatSeq = 0
/** 같은 ms에 두 메시지가 와도 React key가 겹치지 않는 id (L10) */
export function makeChatId(senderId: string, timestamp: number): string {
  chatSeq = (chatSeq + 1) % Number.MAX_SAFE_INTEGER
  return `${senderId}-${timestamp}-${chatSeq}`
}

/** 최근 MAX_CHAT_MESSAGES개만 남긴다 */
export function appendChat(list: ChatMessage[], msg: ChatMessage): ChatMessage[] {
  return [...list, msg].slice(-MAX_CHAT_MESSAGES)
}
