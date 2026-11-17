/**
 * 라우트 입력 스키마 경계 시험 (zod 3 → 4 업그레이드, chore/zod4).
 * 경계 입력(빠짐·타입 틀림·알 수 없는 키·UUID 변형·목록 밖 값·길이 상한·기본값·catch)마다 파싱 결과를 고정한다:
 * 성공이면 정규화된 data, 실패면 게시글 스키마는 클라이언트가 받는 issues(postFieldIssues), 그 밖은 실패 표시만
 * (라우트가 고정 문구를 돌려준다). 기대값 `fixtures/schemas-boundary.json`은 **zod 3.25.76에서 만든 값**이다 —
 * zod 4에서도 같은 값이어야 한다(서버 검증·클라이언트가 받는 오류 형식 불변).
 * 다시 만들기: SCHEMAS_UPDATE_FIXTURE=1 npm test (의도한 변경일 때만, PR 본문에 차이를 적는다)
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import fs from 'fs'
import path from 'path'
import {
  avatarUploadBody, cancelPaymentBody, changePlanBody, confirmPaymentBody, createOrderBody, createPostBody, createRoomBody, orderIdParams,
  postFieldIssues, postIdParams, postImageCompleteBody, postImageResolveBody, postImageUploadBody, roomIdParams, separationIdParams,
  setRoomPasswordBody, updatePostBody, uploadFilenameParams,
} from '../src/routes/schemas'

type Schema = { safeParse: (v: unknown) => { success: true; data: unknown } | { success: false; error: unknown } }

const UUIDS = {
  v4: '3f2a1c9e-1111-4a2b-9c3d-123456789abc',
  nil: '00000000-0000-0000-0000-000000000000',
  // 버전·변형 비트가 RFC 9562 밖(예전 데이터·시험 고정값에 흔함) — zod 3 uuid는 받았다
  nonRfc: '12345678-1234-1234-1234-123456789abc',
  upper: '3F2A1C9E-1111-4A2B-9C3D-123456789ABC',
  short: '3f2a1c9e-1111-4a2b-9c3d-123456789ab',
  junk: 'not-a-uuid',
}

const long = (n: number, c = 'a') => c.repeat(n)

/** [이름, 스키마, 입력, 게시글 스키마 여부(실패 시 issues 기록)] */
const CASES: [string, Schema, unknown, boolean?][] = [
  // 결제 (금액·신원은 서버가 정한다 — 알 수 없는 키는 버림)
  ['confirm ok + extra keys dropped', confirmPaymentBody, { paymentKey: 'pk', orderId: 'order_123', planType: 'pro', amount: 9900, userId: 'evil', price: 1 }],
  ['confirm amount float', confirmPaymentBody, { paymentKey: 'pk', orderId: 'order_123', planType: 'pro', amount: 1.5 }],
  ['confirm amount zero', confirmPaymentBody, { paymentKey: 'pk', orderId: 'order_123', planType: 'pro', amount: 0 }],
  ['confirm amount string', confirmPaymentBody, { paymentKey: 'pk', orderId: 'order_123', planType: 'pro', amount: '9900' }],
  ['confirm orderId short', confirmPaymentBody, { paymentKey: 'pk', orderId: 'ab', planType: 'pro', amount: 1 }],
  ['confirm orderId bad char', confirmPaymentBody, { paymentKey: 'pk', orderId: 'order 123', planType: 'pro', amount: 1 }],
  ['confirm paymentKey too long', confirmPaymentBody, { paymentKey: long(201), orderId: 'order_123', planType: 'pro', amount: 1 }],
  ['confirm planType free', confirmPaymentBody, { paymentKey: 'pk', orderId: 'order_123', planType: 'free', amount: 1 }],
  ['confirm empty', confirmPaymentBody, {}],
  ['createOrder ok', createOrderBody, { planType: 'standard' }],
  ['createOrder bad', createOrderBody, { planType: 'enterprise' }],
  ['orderIdParams ok', orderIdParams, { orderId: 'A-b_123456' }],
  ['orderIdParams too long', orderIdParams, { orderId: long(65) }],
  ['cancel empty', cancelPaymentBody, {}],
  ['cancel reason wrong type → catch', cancelPaymentBody, { paymentKey: 'pk', cancelReason: 42 }],
  ['cancel reason ok', cancelPaymentBody, { cancelReason: '단순 변심' }],
  ['cancel paymentKey too long', cancelPaymentBody, { paymentKey: long(201) }],
  ['changePlan free', changePlanBody, { newPlan: 'free' }],
  ['changePlan bad', changePlanBody, { newPlan: 'gold' }],
  // 업로드·분리
  ['uploadFilename ok', uploadFilenameParams, { filename: 'song.mp3' }],
  ['uploadFilename empty', uploadFilenameParams, { filename: '' }],
  ['uploadFilename 256', uploadFilenameParams, { filename: long(256) }],
  ...Object.entries(UUIDS).map(([k, v]) => [`separationId ${k}`, separationIdParams, { id: v }] as [string, Schema, unknown]),
  ...Object.entries(UUIDS).map(([k, v]) => [`roomId ${k}`, roomIdParams, { id: v }] as [string, Schema, unknown]),
  ...Object.entries(UUIDS).map(([k, v]) => [`postId ${k}`, postIdParams, { id: v }] as [string, Schema, unknown]),
  // 방
  ['createRoom minimal (defaults)', createRoomBody, { title: '  합주방  ' }],
  ['createRoom full', createRoomBody, {
    title: 'Jam', description: '  ', genre: ' rock ', maxParticipants: 6, tags: [' a ', 'b'], freeJoin: false, isPrivate: true,
    instrumentSlots: [{ instrument: 'guitar', count: 2 }, { instrument: 'bass', count: 0 }], password: 'pw', hostId: 'evil',
  }],
  ['createRoom description null', createRoomBody, { title: 'x', description: null, genre: null }],
  ['createRoom blank title', createRoomBody, { title: '   ' }],
  ['createRoom title 101', createRoomBody, { title: long(101) }],
  ['createRoom capacity too big', createRoomBody, { title: 'x', maxParticipants: 999 }],
  ['createRoom capacity float', createRoomBody, { title: 'x', maxParticipants: 5.5 }],
  ['createRoom tag empty after trim', createRoomBody, { title: 'x', tags: ['  '] }],
  ['createRoom 21 tags', createRoomBody, { title: 'x', tags: Array.from({ length: 21 }, (_, i) => `t${i}`) }],
  ['createRoom slot count 11', createRoomBody, { title: 'x', instrumentSlots: [{ instrument: 'g', count: 11 }] }],
  ['createRoom password null', createRoomBody, { title: 'x', password: null }],
  ['createRoom password 257', createRoomBody, { title: 'x', password: long(257) }],
  ['setRoomPassword null', setRoomPasswordBody, { password: null }],
  ['setRoomPassword empty', setRoomPasswordBody, { password: '' }],
  ['setRoomPassword missing', setRoomPasswordBody, {}],
  // 이미지·프로필
  ['postImageUpload ok', postImageUploadBody, { contentType: 'image/gif', size: 1000 }],
  ['postImageUpload svg', postImageUploadBody, { contentType: 'image/svg+xml', size: 1000 }],
  ['postImageUpload size 0', postImageUploadBody, { contentType: 'image/png', size: 0 }],
  ['postImageUpload size huge', postImageUploadBody, { contentType: 'image/png', size: Number.MAX_SAFE_INTEGER }],
  ['avatarUpload gif', avatarUploadBody, { contentType: 'image/gif', size: 10 }],
  ['avatarUpload ok', avatarUploadBody, { contentType: 'image/webp', size: 10 }],
  ['postImageComplete ok', postImageCompleteBody, { key: 'a/b.png' }],
  ['postImageComplete empty', postImageCompleteBody, { key: '' }],
  ['postImageResolve ok', postImageResolveBody, { urls: ['https://x/a.png'] }],
  ['postImageResolve 21', postImageResolveBody, { urls: Array.from({ length: 21 }, () => 'u') }],
  // 게시글 (실패 시 클라이언트가 받는 issues를 기록)
  ['createPost minimal (defaults)', createPostBody, { title: ' 제목 ', content: ' 내용 ' }, true],
  ['createPost empty', createPostBody, {}, true],
  ['createPost blank title', createPostBody, { title: '   ', content: 'c' }, true],
  ['createPost title 201', createPostBody, { title: long(201), content: 'c' }, true],
  ['createPost content too long', createPostBody, { title: 't', content: long(20_001) }, true],
  ['createPost 21 tags', createPostBody, { title: 't', content: 'c', tags: Array.from({ length: 21 }, (_, i) => `t${i}`) }, true],
  ['createPost tag too long', createPostBody, { title: 't', content: 'c', tags: [long(51)] }, true],
  ['createPost blank tag', createPostBody, { title: 't', content: 'c', tags: ['  '] }, true],
  ['createPost 6 images', createPostBody, { title: 't', content: 'c', images: ['a', 'b', 'c', 'd', 'e', 'f'] }, true],
  ['createPost empty image', createPostBody, { title: 't', content: 'c', images: [''] }, true],
  ['createPost bad instrument', createPostBody, { title: 't', content: 'c', instrument: 'flute' }, true],
  ['createPost bad category', createPostBody, { title: 't', content: 'c', category: 'spam' }, true],
  ['createPost wrong types', createPostBody, { title: 1, content: true, tags: 'x', images: {} }, true],
  ['createPost title null', createPostBody, { title: null, content: 'c' }, true],
  // 목록 필드에 문자열이 아닌 값(#224 리뷰 L-1: zod 4는 invalid_value, zod 3은 invalid_type → "형식" 문구)
  ['createPost instrument number', createPostBody, { title: 't', content: 'c', instrument: 3 }, true],
  ['createPost category null', createPostBody, { title: 't', content: 'c', category: null }, true],
  ['createPost instrument object', createPostBody, { title: 't', content: 'c', instrument: { v: 'guitar' } }, true],
  ['createPost category array', createPostBody, { title: 't', content: 'c', category: ['general'] }, true],
  ['createPost many issues', createPostBody, { title: '', content: long(20_001), tags: ['', long(60)], instrument: 'x', category: 'y' }, true],
  ['updatePost empty ok', updatePostBody, {}, true],
  ['updatePost legacy values ok', updatePostBody, { title: long(500), tags: Array.from({ length: 30 }, () => 't'), instrument: 'flute' }, true],
  ['updatePost too big', updatePostBody, { title: long(10_001), images: Array.from({ length: 101 }, () => 'u') }, true],
  ['updatePost empty image', updatePostBody, { images: [''] }, true],
  ['updatePost wrong types', updatePostBody, { title: 3, tags: [1] }, true],
]

function run(): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const [name, schema, input, isPost] of CASES) {
    const r = schema.safeParse(input)
    if (r.success) out[name] = { ok: true, data: r.data }
    else out[name] = isPost ? { ok: false, issues: postFieldIssues(r.error as Parameters<typeof postFieldIssues>[0], input) } : { ok: false }
  }
  return out
}

const FIXTURE = path.join(__dirname, 'fixtures', 'schemas-boundary.json')

describe('route input schemas (boundary golden — generated with zod 3)', () => {
  it('parses every boundary case exactly like the fixture', () => {
    const got = JSON.parse(JSON.stringify(run()))
    if (process.env.SCHEMAS_UPDATE_FIXTURE) {
      fs.writeFileSync(FIXTURE, JSON.stringify(got, null, 2) + '\n')
      return
    }
    const want = JSON.parse(fs.readFileSync(FIXTURE, 'utf8'))
    for (const name of Object.keys(want)) assert.deepEqual(got[name], want[name], name)
    assert.deepEqual(Object.keys(got).sort(), Object.keys(want).sort())
  })
})
