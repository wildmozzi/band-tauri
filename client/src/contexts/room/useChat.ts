import { useCallback, useState } from 'react'
import type { SignalPayload } from './useSignaling'

export interface ChatMessage {
  id: string
  peerId: string
  nickname: string
  message: string
  timestamp: number
}

const MAX_MESSAGES = 500

export function useChat(opts: {
  send: (p: SignalPayload) => void
  canSend: () => boolean
  getSelfId: () => string | null
  nickname: string
}) {
  const { send, canSend, getSelfId, nickname } = opts
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([])

  const append = useCallback((msg: ChatMessage) => setChatMessages((prev) => [...prev, msg].slice(-MAX_MESSAGES)), [])

  const sendChatMessage = useCallback((message: string) => {
    const text = message.trim()
    if (!text || !canSend()) return
    const timestamp = Date.now()
    send({ type: 'chat', message: text, nickname, timestamp })
    const me = getSelfId() ?? 'me'
    append({ id: `${me}-${timestamp}`, peerId: me, nickname, message: text, timestamp })
  }, [send, canSend, getSelfId, nickname, append])

  /** chat 메시지면 처리하고 true. 발신자는 서버가 붙인 oderId만 쓴다 */
  const handleChatSignal = useCallback((payload: SignalPayload): boolean => {
    if (payload.type !== 'chat') return false
    const senderId = payload.oderId
    if (typeof senderId !== 'string' || typeof payload.message !== 'string') return true
    const timestamp = typeof payload.timestamp === 'number' ? payload.timestamp : Date.now()
    append({
      id: `${senderId}-${timestamp}`,
      peerId: senderId,
      nickname: typeof payload.nickname === 'string' ? payload.nickname : `User ${senderId.slice(0, 4)}`,
      message: payload.message,
      timestamp,
    })
    return true
  }, [append])

  const clearChat = useCallback(() => setChatMessages([]), [])

  return { chatMessages, sendChatMessage, handleChatSignal, clearChat }
}
