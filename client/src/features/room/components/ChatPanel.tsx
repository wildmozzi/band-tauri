import { useEffect, useRef } from 'react'
import { useRoom } from '../state/RoomContext'

/** 오른쪽: 채팅. 입력 중인 글은 페이지가 갖는다(재연결 화면을 거쳐도 유지) */
export function ChatPanel({ chatInput, onChatInputChange }: { chatInput: string; onChatInputChange: (v: string) => void }) {
  const { chatMessages, sendChatMessage, nickname, clientId } = useRoom()
  const chatContainerRef = useRef<HTMLDivElement | null>(null)

  // 채팅 스크롤 자동 이동
  useEffect(() => {
    if (chatContainerRef.current) {
      chatContainerRef.current.scrollTop = chatContainerRef.current.scrollHeight
    }
  }, [chatMessages])

  // 채팅 전송
  const handleSendChat = (e: React.FormEvent) => {
    e.preventDefault()
    if (chatInput.trim()) {
      sendChatMessage(chatInput)
      onChatInputChange('')
    }
  }

  return (
    <aside className="chat-panel">
      <div className="panel-header">
        <h2>💬 채팅</h2>
        <div className="nickname-area">
          <span className="nickname-display">{nickname}</span>
        </div>
      </div>

      <div className="chat-messages" ref={chatContainerRef}>
        {chatMessages.length === 0 ? (
          <div className="chat-empty">
            <p>채팅을 시작해보세요!</p>
          </div>
        ) : (
          chatMessages.map((msg) => (
            <div
              key={msg.id}
              className={`chat-message ${msg.peerId === clientId || msg.peerId === 'me' ? 'me' : ''}`}
            >
              <span className="chat-nickname">{msg.nickname}</span>
              <span className="chat-text">{msg.message}</span>
            </div>
          ))
        )}
      </div>

      <form className="chat-input-area" onSubmit={handleSendChat}>
        <input
          type="text"
          placeholder="메시지 입력..."
          value={chatInput}
          onChange={(e) => onChatInputChange(e.target.value)}
          className="chat-input"
        />
        <button type="submit" className="chat-send-btn">전송</button>
      </form>
    </aside>
  )
}
