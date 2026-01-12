import pg from 'pg'
import { readFileSync } from 'fs'
import { fileURLToPath } from 'url'
import { dirname, join } from 'path'

const { Client } = pg

const __filename = fileURLToPath(import.meta.url)
const __dirname = dirname(__filename)

// Supabase PostgreSQL 연결 설정 (Transaction Pooler)
const connectionString = 'postgresql://postgres.agpgkzgkodudpnxzbhcv:Qnsgxbmz4B5loaz5@aws-1-ap-northeast-2.pooler.supabase.com:6543/postgres'

async function runMigrations() {
  const client = new Client({ connectionString })

  try {
    console.log('🔌 Supabase 데이터베이스에 연결 중...')
    await client.connect()
    console.log('✅ 연결 성공!\n')

    // 마이그레이션 001: 테이블 생성
    console.log('📋 마이그레이션 001: 테이블 생성 실행 중...')
    const migration001 = readFileSync(
      join(__dirname, '문서', 'migrations', '001_create_tables.sql'),
      'utf-8'
    )
    await client.query(migration001)
    console.log('✅ 마이그레이션 001 완료!\n')

    // 마이그레이션 002: RLS 정책 설정
    console.log('🔒 마이그레이션 002: RLS 정책 설정 실행 중...')
    const migration002 = readFileSync(
      join(__dirname, '문서', 'migrations', '002_setup_rls.sql'),
      'utf-8'
    )
    await client.query(migration002)
    console.log('✅ 마이그레이션 002 완료!\n')

    // 테이블 확인
    console.log('📊 생성된 테이블 확인 중...')
    const result = await client.query(`
      SELECT table_name
      FROM information_schema.tables
      WHERE table_schema = 'public'
      ORDER BY table_name;
    `)

    console.log('생성된 테이블:')
    result.rows.forEach(row => {
      console.log(`  ✅ ${row.table_name}`)
    })

    console.log('\n🎉 모든 마이그레이션이 성공적으로 완료되었습니다!')

  } catch (err) {
    console.error('❌ 에러 발생:', err.message)
    console.error(err)
    process.exit(1)
  } finally {
    await client.end()
    console.log('\n🔌 데이터베이스 연결 종료')
  }
}

runMigrations()
