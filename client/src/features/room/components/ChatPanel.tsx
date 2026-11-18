import { useEffect, useRef } from 'react'
import { Link } from 'react-router-dom'
import { useAuth } from '../../auth'
import { useRoomConnection, useRoomChat } from '../state/roomHooks'
import './ChatPanel.css'

/**
 * 오른쪽: 채팅. 입력 중인 글은 페이지가 갖는다(재연결 화면을 거쳐도 유지).
 * 로그인하지 않은 관람자는 읽기만 한다(P2-14c — 서버가 익명 채팅을 chat-login-required로 거절): 입력창 대신 로그인 안내.
 * 세션 복원 중(loading)에는 입력창을 그대로 둔다(로그인 사용자가 새로고침할 때 안내가 깜박이지 않게 — 서버가 최종 판정)
 */
export function ChatPanel({ chatInput, onChatInputChange }: { chatInput: string; onChatInputChange: (v: string) => void }) {
  const { nickname, clientId } = useRoomConnection()
  const { chatMessages, chatLoginRequired, sendChatMessage } = useRoomChat()
  const { user, loading } = useAuth()
  const loginRequired = (!loading && !user) || chatLoginRequired
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
            <p>{loginRequired ? '아직 채팅이 없어요' : '채팅을 시작해보세요!'}</p>
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

      {loginRequired ? (
        <div className="chat-input-area chat-login-notice" role="note">
          <span>로그인하면 채팅할 수 있어요</span>
          <Link to="/auth" className="chat-login-link">로그인</Link>
        </div>
      ) : (
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
      )}
    </aside>
  )
}
