import { createClient } from '@supabase/supabase-js'

const supabaseUrl = 'https://agpgkzgkodudpnxzbhcv.supabase.co'
const supabaseKey = 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImFncGdremdrb2R1ZHBueHpiaGN2Iiwicm9sZSI6ImFub24iLCJpYXQiOjE3NjI5OTk3MDAsImV4cCI6MjA3ODU3NTcwMH0.lIidV7ZNon9ygr9dfuClWNVDJ3w0jHw14UBpK0K4S3w'

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
