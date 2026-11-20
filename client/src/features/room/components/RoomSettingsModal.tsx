import { AVAILABLE_INSTRUMENTS, COMMON_TAGS, GENRES, maxParticipantOptionsFor, MAX_PARTICIPANTS_HINT, MAX_PARTICIPANTS_LABEL, maxParticipantsOptionLabel } from '../../rooms'
import type { DetailRoom } from '../roomDetailModel'
import type { RoomSettingsForm } from '../hooks/useRoomSettingsForm'
import { Modal } from '../../../shared/ui/Modal'
import { Field } from '../../../shared/ui/Field'
import './RoomSettingsModal.css'

/** 방 설정 모달 (방장용): 제목·설명·장르·인원·태그·악기 구성·참여 방식·공개 여부·비밀번호 */
export function RoomSettingsModal({ room, form }: { room: DetailRoom; form: RoomSettingsForm }) {
  const { editTags, editInstrumentSlots, availableToAdd, editFreeJoin, editIsPrivate, editPasswordMode, settingsSaving } = form

  return (
    <Modal className="room-edit-modal" title="합주실 설정" onClose={form.closeRoomSettings}>
      <Field id="edit-title" label="방 제목 *" className="edit-form-group">
        <input
          id="edit-title"
          type="text"
          value={form.editTitle}
          onChange={(e) => form.setEditTitle(e.target.value)}
          placeholder="예: 주말 재즈 세션"
          maxLength={100}
        />
      </Field>

      <Field id="edit-description" label="설명" className="edit-form-group">
        <textarea
          id="edit-description"
          value={form.editDescription}
          onChange={(e) => form.setEditDescription(e.target.value)}
          rows={3}
          maxLength={500}
          placeholder="어떤 합주를 하고 싶으신가요? (선택사항)"
        />
      </Field>

      <div className="edit-form-row">
        <Field id="edit-genre" label="장르" className="edit-form-group">
          <select
            id="edit-genre"
            value={form.editGenre}
            onChange={(e) => form.setEditGenre(e.target.value)}
          >
            {GENRES.map(g => (
              <option key={g} value={g}>{g}</option>
            ))}
          </select>
        </Field>

        <div className="edit-form-group">
          <label htmlFor="edit-max">{MAX_PARTICIPANTS_LABEL}</label>
          <select
            id="edit-max"
            value={form.editMaxParticipants}
            aria-describedby="edit-max-hint"
            onChange={(e) => form.setEditMaxParticipants(Number(e.target.value))}
          >
            {maxParticipantOptionsFor(form.editMaxParticipants).map(n => (
              <option key={n} value={n}>{maxParticipantsOptionLabel(n)}</option>
            ))}
          </select>
          <small id="edit-max-hint" className="form-hint">{MAX_PARTICIPANTS_HINT}</small>
        </div>
      </div>

      {/* 태그 */}
      <div className="edit-form-group">
        <label>태그</label>
        <div className="edit-tags-container">
          {COMMON_TAGS.map(tag => (
            <button
              key={tag}
              type="button"
              className={`edit-tag-btn ${editTags.includes(tag) ? 'active' : ''}`}
              onClick={() => form.handleEditTagToggle(tag)}
            >
              {tag}
            </button>
          ))}
        </div>
        <div className="edit-custom-tag-input">
          <input
            type="text"
            placeholder="커스텀 태그 추가"
            value={form.editCustomTag}
            onChange={(e) => form.setEditCustomTag(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), form.handleAddEditCustomTag())}
            maxLength={20}
          />
          <button type="button" onClick={form.handleAddEditCustomTag}>추가</button>
        </div>
        {editTags.length > 0 && (
          <div className="edit-selected-tags">
            {editTags.map(tag => (
              <span key={tag} className="edit-selected-tag">
                {tag}
                <button type="button" onClick={() => form.handleEditTagToggle(tag)}>×</button>
              </span>
            ))}
          </div>
        )}
      </div>

      {/* 악기 구성 */}
      <div className="edit-form-group">
        <label>악기 구성</label>
        <p className="edit-form-hint">각 악기별 참여 가능 인원을 설정하세요 (총 {form.editTotalSlots}자리)</p>

        <div className="edit-instrument-slots">
          {editInstrumentSlots.map(slot => {
            const instInfo = AVAILABLE_INSTRUMENTS.find(i => i.id === slot.instrument)
            return (
              <div key={slot.instrument} className="edit-instrument-slot">
                <span className="edit-slot-icon">{instInfo?.icon}</span>
                <span className="edit-slot-name">{instInfo?.name || slot.instrument}</span>
                <div className="edit-slot-count-control">
                  <button
                    type="button"
                    onClick={() => form.handleSlotCountChange(slot.instrument, slot.count - 1)}
                    disabled={slot.count <= 0}
                  >
                    -
                  </button>
                  <span className="edit-slot-count">{slot.count}</span>
                  <button
                    type="button"
                    onClick={() => form.handleSlotCountChange(slot.instrument, slot.count + 1)}
                    disabled={slot.count >= 10}
                  >
                    +
                  </button>
                </div>
                <button
                  type="button"
                  className="edit-slot-remove"
                  onClick={() => form.handleRemoveInstrument(slot.instrument)}
                  title="악기 제거"
                >
                  ×
                </button>
              </div>
            )
          })}
        </div>

        {availableToAdd.length > 0 && (
          <div className="edit-add-instrument">
            <select
              onChange={(e) => {
                if (e.target.value) {
                  form.handleAddInstrument(e.target.value)
                  e.target.value = ''
                }
              }}
              defaultValue=""
            >
              <option value="" disabled>+ 악기 추가</option>
              {availableToAdd.map(inst => (
                <option key={inst.id} value={inst.id}>
                  {inst.icon} {inst.name}
                </option>
              ))}
            </select>
          </div>
        )}
      </div>

      {/* 참여 방식 */}
      <div className="edit-form-group">
        <label>참여 방식</label>
        <div className="edit-toggle-option">
          <label className="toggle-switch">
            <input
              type="checkbox"
              role="switch"
              aria-label="자유 참여"
              checked={editFreeJoin}
              onChange={(e) => form.setEditFreeJoin(e.target.checked)}
            />
            <span className="toggle-slider"></span>
          </label>
          <div className="toggle-label">
            <strong>{editFreeJoin ? '자유 참여' : '승인 필요'}</strong>
            <span>{editFreeJoin ? '누구나 바로 연주자로 참여할 수 있습니다' : '방장이 승인해야 연주자로 참여할 수 있습니다'}</span>
          </div>
        </div>
      </div>

      {/* 공개 여부 (마이그레이션 008 이후) */}
      {room.is_private !== undefined && (
        <div className="edit-form-group">
          <label>공개 여부</label>
          <div className="edit-toggle-option">
            <label className="toggle-switch">
              <input
                type="checkbox"
                role="switch"
                aria-label="합주실 목록에 공개"
                checked={!editIsPrivate}
                onChange={(e) => form.setEditIsPrivate(!e.target.checked)}
              />
              <span className="toggle-slider"></span>
            </label>
            <div className="toggle-label">
              <strong>{editIsPrivate ? '비공개' : '공개'}</strong>
              <span>{editIsPrivate ? '목록에 보이지 않습니다. 초대 링크/방 코드로만 들어올 수 있습니다' : '합주실 목록에 표시됩니다'}</span>
            </div>
          </div>
        </div>
      )}

      {/* 비밀번호 (마이그레이션 008 이후) */}
      {room.has_password !== undefined && (
      <div className="edit-form-group">
        <label>비밀번호 {room.has_password ? '(설정됨)' : '(없음)'}</label>
        <div className="password-mode-options">
          <label><input type="radio" checked={editPasswordMode === 'keep'} onChange={() => form.setEditPasswordMode('keep')} /> 그대로</label>
          <label><input type="radio" checked={editPasswordMode === 'set'} onChange={() => form.setEditPasswordMode('set')} /> {room.has_password ? '변경' : '설정'}</label>
          {room.has_password && (
            <label><input type="radio" checked={editPasswordMode === 'clear'} onChange={() => form.setEditPasswordMode('clear')} /> 해제</label>
          )}
        </div>
        {editPasswordMode === 'set' && (
          <input
            type="password"
            autoComplete="new-password"
            placeholder="6~64자"
            value={form.editPassword}
            onChange={(e) => form.setEditPassword(e.target.value)}
            maxLength={64}
          />
        )}
        <small className="form-hint">이미 들어와 있는 참여자는 그대로 남습니다. 새로 들어오는 사람부터 적용됩니다.</small>
      </div>
      )}

      <div className="edit-form-actions">
        <button
          onClick={form.closeRoomSettings}
          className="edit-cancel-btn"
          disabled={settingsSaving}
        >
          취소
        </button>
        <button
          onClick={form.handleSaveRoomSettings}
          className="edit-save-btn"
          disabled={settingsSaving}
        >
          {settingsSaving ? '저장 중...' : '저장'}
        </button>
      </div>
    </Modal>
  )
}
