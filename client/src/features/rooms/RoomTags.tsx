import { findInstrument, type InstrumentSlot } from './roomModel'

interface TagOverflowProps {
  roomId: string
  tags: string[]
  /** 'room-card-tag' | 'my-room-tag' */
  tagClassName: string
  expanded: boolean
  onToggle: (e: React.MouseEvent, roomId: string) => void
}

/** 앞 3개 태그 + "+N" 칩(클릭 시 나머지 팝업). */
export function TagOverflow({ roomId, tags, tagClassName, expanded, onToggle }: TagOverflowProps) {
  return (
    <>
      {tags.slice(0, 3).map(tag => (
        <span key={tag} className={tagClassName}>{tag}</span>
      ))}
      {tags.length > 3 && (
        <span
          className={`${tagClassName} more clickable`}
          onClick={(e) => onToggle(e, roomId)}
        >
          +{tags.length - 3}
          {expanded && (
            <div className="tags-popup">
              {tags.slice(3).map(tag => (
                <span key={tag} className="popup-tag">{tag}</span>
              ))}
            </div>
          )}
        </span>
      )}
    </>
  )
}

interface InstrumentBadgesProps {
  slots: InstrumentSlot[]
  /** 'room-card-instruments' | 'my-room-instruments' */
  className: string
  /** 'room-card-inst' | 'my-room-inst' */
  itemClassName: string
}

/** 0보다 큰 악기 슬롯을 아이콘 + "0/N"으로 표시. */
export function InstrumentBadges({ slots, className, itemClassName }: InstrumentBadgesProps) {
  return (
    <div className={className}>
      {slots.filter(s => s.count > 0).map(slot => {
        const inst = findInstrument(slot.instrument)
        return (
          <span key={slot.instrument} className={itemClassName} title={inst?.name}>
            {inst?.icon} <span className="inst-slot-count">0/{slot.count}</span>
          </span>
        )
      })}
    </div>
  )
}
