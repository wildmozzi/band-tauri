import pg from 'pg'

const { Client } = pg

// DB 접속 문자열은 환경변수로만 받는다 (루트 .env.example 참고)
const connectionString = process.env.DATABASE_URL
if (!connectionString) {
  console.error('❌ DATABASE_URL 환경변수가 필요합니다. 예) DATABASE_URL=postgresql://... node setup-rls.js')
  process.exit(1)
}

const rlsQueries = [
  // RLS 활성화
  {
    name: 'RLS 활성화',
    sql: `
      ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;
      ALTER TABLE rooms ENABLE ROW LEVEL SECURITY;
      ALTER TABLE posts ENABLE ROW LEVEL SECURITY;
      ALTER TABLE comments ENABLE ROW LEVEL SECURITY;
      ALTER TABLE marketplace ENABLE ROW LEVEL SECURITY;
      ALTER TABLE recordings ENABLE ROW LEVEL SECURITY;
      ALTER TABLE messages ENABLE ROW LEVEL SECURITY;
    `
  },
  // profiles 정책
  {
    name: 'profiles 정책',
    sql: `
      DROP POLICY IF EXISTS "profiles_select_all" ON profiles;
      CREATE POLICY "profiles_select_all" ON profiles FOR SELECT USING (true);

      DROP POLICY IF EXISTS "profiles_update_own" ON profiles;
      CREATE POLICY "profiles_update_own" ON profiles FOR UPDATE USING (auth.uid() = id);

      DROP POLICY IF EXISTS "profiles_delete_own" ON profiles;
      CREATE POLICY "profiles_delete_own" ON profiles FOR DELETE USING (auth.uid() = id);

      DROP POLICY IF EXISTS "profiles_insert_own" ON profiles;
      CREATE POLICY "profiles_insert_own" ON profiles FOR INSERT WITH CHECK (auth.uid() = id);
    `
  },
  // rooms 정책
  {
    name: 'rooms 정책',
    sql: `
      DROP POLICY IF EXISTS "rooms_select_all" ON rooms;
      CREATE POLICY "rooms_select_all" ON rooms FOR SELECT USING (true);

      DROP POLICY IF EXISTS "rooms_insert_authenticated" ON rooms;
      CREATE POLICY "rooms_insert_authenticated" ON rooms FOR INSERT WITH CHECK (auth.role() = 'authenticated');

      DROP POLICY IF EXISTS "rooms_update_host" ON rooms;
      CREATE POLICY "rooms_update_host" ON rooms FOR UPDATE USING (auth.uid() = host_id);

      DROP POLICY IF EXISTS "rooms_delete_host" ON rooms;
      CREATE POLICY "rooms_delete_host" ON rooms FOR DELETE USING (auth.uid() = host_id);
    `
  },
  // posts 정책
  {
    name: 'posts 정책',
    sql: `
      DROP POLICY IF EXISTS "posts_select_all" ON posts;
      CREATE POLICY "posts_select_all" ON posts FOR SELECT USING (true);

      DROP POLICY IF EXISTS "posts_insert_authenticated" ON posts;
      CREATE POLICY "posts_insert_authenticated" ON posts FOR INSERT WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);

      DROP POLICY IF EXISTS "posts_update_author" ON posts;
      CREATE POLICY "posts_update_author" ON posts FOR UPDATE USING (auth.uid() = author_id);

      DROP POLICY IF EXISTS "posts_delete_author" ON posts;
      CREATE POLICY "posts_delete_author" ON posts FOR DELETE USING (auth.uid() = author_id);
    `
  },
  // comments 정책
  {
    name: 'comments 정책',
    sql: `
      DROP POLICY IF EXISTS "comments_select_all" ON comments;
      CREATE POLICY "comments_select_all" ON comments FOR SELECT USING (true);

      DROP POLICY IF EXISTS "comments_insert_authenticated" ON comments;
      CREATE POLICY "comments_insert_authenticated" ON comments FOR INSERT WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);

      DROP POLICY IF EXISTS "comments_update_author" ON comments;
      CREATE POLICY "comments_update_author" ON comments FOR UPDATE USING (auth.uid() = author_id);

      DROP POLICY IF EXISTS "comments_delete_author" ON comments;
      CREATE POLICY "comments_delete_author" ON comments FOR DELETE USING (auth.uid() = author_id);
    `
  },
  // marketplace 정책
  {
    name: 'marketplace 정책',
    sql: `
      DROP POLICY IF EXISTS "marketplace_select_all" ON marketplace;
      CREATE POLICY "marketplace_select_all" ON marketplace FOR SELECT USING (true);

      DROP POLICY IF EXISTS "marketplace_insert_authenticated" ON marketplace;
      CREATE POLICY "marketplace_insert_authenticated" ON marketplace FOR INSERT WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = seller_id);

      DROP POLICY IF EXISTS "marketplace_update_seller" ON marketplace;
      CREATE POLICY "marketplace_update_seller" ON marketplace FOR UPDATE USING (auth.uid() = seller_id);

      DROP POLICY IF EXISTS "marketplace_delete_seller" ON marketplace;
      CREATE POLICY "marketplace_delete_seller" ON marketplace FOR DELETE USING (auth.uid() = seller_id);
    `
  },
  // recordings 정책
  {
    name: 'recordings 정책',
    sql: `
      DROP POLICY IF EXISTS "recordings_select_public_or_own" ON recordings;
      CREATE POLICY "recordings_select_public_or_own" ON recordings FOR SELECT USING (is_public = true OR auth.uid() = user_id);

      DROP POLICY IF EXISTS "recordings_insert_authenticated" ON recordings;
      CREATE POLICY "recordings_insert_authenticated" ON recordings FOR INSERT WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = user_id);

      DROP POLICY IF EXISTS "recordings_update_own" ON recordings;
      CREATE POLICY "recordings_update_own" ON recordings FOR UPDATE USING (auth.uid() = user_id);

      DROP POLICY IF EXISTS "recordings_delete_own" ON recordings;
      CREATE POLICY "recordings_delete_own" ON recordings FOR DELETE USING (auth.uid() = user_id);
    `
  },
  // messages 정책
  {
    name: 'messages 정책',
    sql: `
      DROP POLICY IF EXISTS "messages_select_participants" ON messages;
      CREATE POLICY "messages_select_participants" ON messages FOR SELECT USING (auth.uid() = sender_id OR auth.uid() = receiver_id);

      DROP POLICY IF EXISTS "messages_insert_authenticated" ON messages;
      CREATE POLICY "messages_insert_authenticated" ON messages FOR INSERT WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = sender_id);

      DROP POLICY IF EXISTS "messages_update_receiver" ON messages;
      CREATE POLICY "messages_update_receiver" ON messages FOR UPDATE USING (auth.uid() = receiver_id);

      DROP POLICY IF EXISTS "messages_delete_sender" ON messages;
      CREATE POLICY "messages_delete_sender" ON messages FOR DELETE USING (auth.uid() = sender_id);
    `
  }
]

async function setupRLS() {
  const client = new Client({ connectionString })

  try {
    console.log('🔌 Supabase 데이터베이스에 연결 중...')
    await client.connect()
    console.log('✅ 연결 성공!\n')

    for (const query of rlsQueries) {
      console.log(`🔒 ${query.name} 설정 중...`)
      try {
        await client.query(query.sql)
        console.log(`  ✅ ${query.name} 완료`)
      } catch (err) {
        console.log(`  ⚠️ ${query.name} 오류: ${err.message}`)
      }
    }

    console.log('\n🎉 RLS 정책 설정 완료!')

  } catch (err) {
    console.error('❌ 연결 에러:', err.message)
    process.exit(1)
  } finally {
    await client.end()
    console.log('\n🔌 데이터베이스 연결 종료')
  }
}

setupRLS()
