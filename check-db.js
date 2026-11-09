import { createClient } from '@supabase/supabase-js'

const supabaseUrl = process.env.SUPABASE_URL
const supabaseKey = process.env.SUPABASE_ANON_KEY
if (!supabaseUrl || !supabaseKey) {
  console.error('❌ SUPABASE_URL, SUPABASE_ANON_KEY 환경변수가 필요합니다. (루트 .env.example 참고)')
  process.exit(1)
}

const supabase = createClient(supabaseUrl, supabaseKey)

async function checkDatabase() {
  console.log('🔍 Supabase 데이터베이스 전체 확인 중...\n')

  const tables = ['profiles', 'rooms', 'posts', 'comments', 'marketplace', 'recordings', 'messages']

  for (const table of tables) {
    console.log(`📋 ${table} 테이블:`)
    const { data, error } = await supabase
      .from(table)
      .select('*')
      .limit(3)

    if (error) {
      console.log(`  ❌ 에러: ${error.message}`)
    } else {
      console.log(`  ✅ 테이블 존재 (${data.length}개 레코드)`)
      if (data.length > 0) {
        console.log(`  컬럼: ${Object.keys(data[0]).join(', ')}`)
      }
    }
    console.log('')
  }
}

checkDatabase().catch(console.error)
