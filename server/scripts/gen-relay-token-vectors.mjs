// 릴레이 토큰·Bind MAC 공용 벡터(docs/overhaul/relay-token-vectors.json) 생성기 — relay-design 2.1.1·3절.
// 서버·릴레이·엔진 구현을 쓰지 않고 node:crypto만으로 독립 계산한다(교차 검증용).
//
//   node server/scripts/gen-relay-token-vectors.mjs          # 입력 필드로 hex·bindKeyHex·bind.hex를 다시 계산해 비교(불일치면 exit 1)
//   node server/scripts/gen-relay-token-vectors.mjs --write  # 다시 계산한 값으로 파일을 덮어쓴다(벡터 추가·수정 때)
//
// 입력 필드: name, secret, roomId, memberId, ssrc, expiresAt, bind.{seq, timestamp, cookieHex}.
import { createHmac } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const FILE = resolve(dirname(fileURLToPath(import.meta.url)), '../../docs/overhaul/relay-token-vectors.json')
const hmac = (key, data) => createHmac('sha256', key).update(data).digest()

function u32(n) {
  const b = Buffer.alloc(4)
  b.writeUInt32BE(n >>> 0)
  return b
}

// 토큰: version(1) | expires_at(u64) | ssrc(u32) | room_len | room | member_len | member | HMAC(secret, 앞 전부)
function token(v) {
  const room = Buffer.from(v.roomId, 'utf8')
  const member = Buffer.from(v.memberId, 'utf8')
  const exp = Buffer.alloc(8)
  exp.writeBigUInt64BE(BigInt(v.expiresAt))
  const body = Buffer.concat([Buffer.from([1]), exp, u32(v.ssrc), Buffer.from([room.length]), room, Buffer.from([member.length]), member])
  return Buffer.concat([body, hmac(v.secret, body)])
}

// bindKey = HMAC(HMAC(secret, "band-relay bind v1"), 토큰 전체)
const bindKey = (secret, tok) => hmac(hmac(secret, 'band-relay bind v1'), tok)

// RelayBind: magic "BN" | ver 1 | type 7 | ssrc | seq | timestamp | flags=1(MAC) | 0 | 토큰 | [쿠키 16] | MAC 16
function bind(v, tok, key) {
  const head = Buffer.concat([Buffer.from([0x42, 0x4e, 1, 7]), u32(v.ssrc), u32(v.bind.seq), u32(v.bind.timestamp), Buffer.from([1, 0])])
  const cookie = v.bind.cookieHex ? Buffer.from(v.bind.cookieHex, 'hex') : Buffer.alloc(0)
  const unsigned = Buffer.concat([head, tok, cookie])
  return Buffer.concat([unsigned, hmac(key, unsigned).subarray(0, 16)])
}

const doc = JSON.parse(readFileSync(FILE, 'utf8'))
let mismatches = 0
for (const v of doc.vectors) {
  const tok = token(v)
  const key = bindKey(v.secret, tok)
  const got = { hex: tok.toString('hex'), bindKeyHex: key.toString('hex'), bindHex: bind(v, tok, key).toString('hex') }
  for (const [field, want] of [['hex', v.hex], ['bindKeyHex', v.bindKeyHex], ['bindHex', v.bind.hex]]) {
    if (got[field] !== want) {
      mismatches++
      console.error(`${v.name}: ${field} 불일치`)
    }
  }
  v.hex = got.hex
  v.bindKeyHex = got.bindKeyHex
  v.bind.hex = got.bindHex
}
if (process.argv.includes('--write')) {
  writeFileSync(FILE, JSON.stringify(doc, null, 2) + '\n')
  console.log(`${doc.vectors.length}개 벡터를 썼다: ${FILE}`)
} else if (mismatches > 0) {
  process.exit(1)
} else {
  console.log(`${doc.vectors.length}개 벡터 일치`)
}
