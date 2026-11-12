import { INSTRUMENT_INFO, instrumentInfo, type InstrumentSlot } from '../roomDetailModel'
import { Modal } from '../../../shared/ui/Modal'

/** 악기 선택 모달 (연주 참여) */
export function InstrumentSelectModal({
  slots,
  getInstrumentUsage,
  onSelect,
  onClose,
}: {
  slots: InstrumentSlot[] | undefined
  getInstrumentUsage: (instrumentId: string) => number
  onSelect: (instrumentId: string) => void
  onClose: () => void
}) {
  return (
    <Modal className="instrument-select-modal" title="악기 선택" onClose={onClose}>
      <p className="modal-description">연주할 악기를 선택하세요</p>
      <div className="instrument-grid">
        {slots?.map(slot => {
          const info = instrumentInfo(slot.instrument)
          const used = getInstrumentUsage(slot.instrument)
          const available = slot.count - used
          const isAvailable = available > 0

          return (
            <button
              key={slot.instrument}
              className={`instrument-option ${!isAvailable ? 'disabled' : ''}`}
              onClick={() => isAvailable && onSelect(slot.instrument)}
              disabled={!isAvailable}
            >
              <span className="inst-icon">{info.icon}</span>
              <span className="inst-name">{info.name}</span>
              <span className={`inst-slots ${available === 0 ? 'full' : ''}`}>
                {available}/{slot.count}
              </span>
            </button>
          )
        })}
      </div>
    </Modal>
  )
}

/** 악기 변경 요청 모달 (방장이 아닌 연주자) */
export function InstrumentChangeModal({
  slots,
  myInstrument,
  getInstrumentUsage,
  onRequest,
  onClose,
}: {
  slots: InstrumentSlot[] | undefined
  myInstrument: string | null
  getInstrumentUsage: (instrumentId: string) => number
  onRequest: (instrumentId: string) => void
  onClose: () => void
}) {
  return (
    <Modal className="instrument-select-modal" title="🔄 악기 변경 요청" onClose={onClose}>
      <p className="modal-description">
        현재 악기: <strong>{INSTRUMENT_INFO[myInstrument || '']?.name || myInstrument}</strong>
      </p>
      <p className="modal-description">변경할 악기를 선택하세요 (방장의 승인이 필요합니다)</p>
      <div className="instrument-grid">
        {slots?.map(slot => {
          const info = instrumentInfo(slot.instrument)
          const used = getInstrumentUsage(slot.instrument)
          // 현재 내 악기는 내가 사용 중이므로 1 빼줌
          const myCurrentUsage = slot.instrument === myInstrument ? 1 : 0
          const available = slot.count - used + myCurrentUsage
          const isAvailable = available > 0 && slot.instrument !== myInstrument

          return (
            <button
              key={slot.instrument}
              className={`instrument-option ${!isAvailable ? 'disabled' : ''} ${slot.instrument === myInstrument ? 'current' : ''}`}
              onClick={() => {
                if (isAvailable) {
                  onRequest(slot.instrument)
                  onClose()
                }
              }}
              disabled={!isAvailable}
            >
              <span className="inst-icon">{info.icon}</span>
              <span className="inst-name">{info.name}</span>
              {slot.instrument === myInstrument ? (
                <span className="inst-slots current">현재</span>
              ) : (
                <span className={`inst-slots ${available === 0 ? 'full' : ''}`}>
                  {available}/{slot.count}
                </span>
              )}
            </button>
          )
        })}
      </div>
    </Modal>
  )
}
