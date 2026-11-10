import { ALICE } from './users'

/** GET /api/music/stem-history 응답의 separations (서버 형식 그대로) */
export const STEM_HISTORY = [
  {
    id: 'eeeeeeee-0000-4000-8000-000000000001',
    user_id: ALICE.id,
    original_filename: 'my-band-demo.mp3',
    original_file_size: 4_200_000,
    vocals_url: 'http://supabase.e2e.test/storage/v1/object/public/audio-stems/a/sep1/vocals.ogg',
    drums_url: 'http://supabase.e2e.test/storage/v1/object/public/audio-stems/a/sep1/drums.ogg',
    bass_url: 'http://supabase.e2e.test/storage/v1/object/public/audio-stems/a/sep1/bass.ogg',
    piano_url: null,
    guitar_url: null,
    other_url: 'http://supabase.e2e.test/storage/v1/object/public/audio-stems/a/sep1/other.ogg',
    storage_folder: 'a/sep1',
    status: 'completed',
    created_at: '2026-11-01T08:00:00.000Z',
    expires_at: '2026-12-01T08:00:00.000Z',
  },
  {
    id: 'eeeeeeee-0000-4000-8000-000000000002',
    user_id: ALICE.id,
    original_filename: 'jam-session-take2.wav',
    original_file_size: 31_000_000,
    vocals_url: 'http://supabase.e2e.test/storage/v1/object/public/audio-stems/a/sep2/vocals.ogg',
    drums_url: null,
    bass_url: null,
    piano_url: null,
    guitar_url: 'http://supabase.e2e.test/storage/v1/object/public/audio-stems/a/sep2/guitar.ogg',
    other_url: null,
    storage_folder: 'a/sep2',
    status: 'completed',
    created_at: '2026-10-28T08:00:00.000Z',
    expires_at: '2026-11-27T08:00:00.000Z',
  },
]
