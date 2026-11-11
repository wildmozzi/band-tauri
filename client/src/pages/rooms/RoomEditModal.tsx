import type { RoomEditorState } from './useRoomEditor'
import { COMMON_TAGS, GENRES, maxParticipantOptionsFor, MAX_PARTICIPANTS_HINT, MAX_PARTICIPANTS_LABEL, MAX_SLOT_COUNT, findInstrument, maxParticipantsOptionLabel } from './roomModel'
import { Modal } from '../../components/ui/Modal'
import { Field } from '../../components/ui/Field'

/** "합주실 설정" 모달. editor.editingRoom이 있을 때만 렌더한다. */
export function RoomEditModal({ editor }: { editor: RoomEditorState }) {
  const { editTags, editInstrumentSlots, availableToAdd, editFreeJoin, editSaving } = editor

  return (
    <Modal className="room-edit-modal" title="합주실 설정" onClose={editor.closeEditor}>
      <Field id="edit-title" label="방 제목 *" className="edit-form-group">
        <input
          id="edit-title"
          type="text"
          value={editor.editTitle}
          onChange={(e) => editor.setEditTitle(e.target.value)}
          placeholder="예: 주말 재즈 세션"
          maxLength={100}
        />
      </Field>

      <Field id="edit-description" label="설명" className="edit-form-group">
        <textarea
          id="edit-description"
          value={editor.editDescription}
          onChange={(e) => editor.setEditDescription(e.target.value)}
          rows={3}
          maxLength={500}
          placeholder="어떤 합주를 하고 싶으신가요? (선택사항)"
        />
      </Field>

      <div className="edit-form-row">
        <Field id="edit-genre" label="장르" className="edit-form-group">
          <select
            id="edit-genre"
            value={editor.editGenre}
            onChange={(e) => editor.setEditGenre(e.target.value)}
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
            value={editor.editMaxParticipants}
            aria-describedby="edit-max-hint"
            onChange={(e) => editor.setEditMaxParticipants(Number(e.target.value))}
          >
            {maxParticipantOptionsFor(editor.editMaxParticipants).map(n => (
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
              onClick={() => editor.handleTagToggle(tag)}
            >
              {tag}
            </button>
          ))}
        </div>
        <div className="edit-custom-tag-input">
          <input
            type="text"
            placeholder="커스텀 태그 추가"
            value={editor.editCustomTag}
            onChange={(e) => editor.setEditCustomTag(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), editor.handleAddCustomTag())}
            maxLength={20}
          />
          <button type="button" onClick={editor.handleAddCustomTag}>추가</button>
        </div>
        {editTags.length > 0 && (
          <div className="edit-selected-tags">
            {editTags.map(tag => (
              <span key={tag} className="edit-selected-tag">
                {tag}
                <button type="button" onClick={() => editor.handleTagToggle(tag)}>×</button>
              </span>
            ))}
          </div>
        )}
      </div>

      {/* 악기 구성 */}
      <div className="edit-form-group">
        <label>악기 구성</label>
        <p className="edit-form-hint">각 악기별 참여 가능 인원을 설정하세요 (총 {editor.totalSlots}자리)</p>

        <div className="edit-instrument-slots">
          {editInstrumentSlots.map(slot => {
            const instInfo = findInstrument(slot.instrument)
            return (
              <div key={slot.instrument} className="edit-instrument-slot">
                <span className="edit-slot-icon">{instInfo?.icon}</span>
                <span className="edit-slot-name">{instInfo?.name || slot.instrument}</span>
                <div className="edit-slot-count-control">
                  <button
                    type="button"
                    onClick={() => editor.handleSlotCountChange(slot.instrument, slot.count - 1)}
                    disabled={slot.count <= 0}
                  >
                    -
                  </button>
                  <span className="edit-slot-count">{slot.count}</span>
                  <button
                    type="button"
                    onClick={() => editor.handleSlotCountChange(slot.instrument, slot.count + 1)}
                    disabled={slot.count >= MAX_SLOT_COUNT}
                  >
                    +
                  </button>
                </div>
                <button
                  type="button"
                  className="edit-slot-remove"
                  onClick={() => editor.handleRemoveInstrument(slot.instrument)}
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
                  editor.handleAddInstrument(e.target.value)
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
              checked={editFreeJoin}
              onChange={(e) => editor.setEditFreeJoin(e.target.checked)}
            />
            <span className="toggle-slider"></span>
          </label>
          <div className="toggle-label">
            <strong>{editFreeJoin ? '자유 참여' : '승인 필요'}</strong>
            <span>{editFreeJoin ? '누구나 바로 연주자로 참여할 수 있습니다' : '방장이 승인해야 연주자로 참여할 수 있습니다'}</span>
          </div>
        </div>
      </div>

      <div className="edit-form-actions">
        <button
          onClick={editor.closeEditor}
          className="edit-cancel-btn"
          disabled={editSaving}
        >
          취소
        </button>
        <button
          onClick={editor.handleSave}
          className="edit-save-btn"
          disabled={editSaving}
        >
          {editSaving ? '저장 중...' : '저장'}
        </button>
      </div>
    </Modal>
  )
}
