const fs = require('fs')
const path = require('path')

// 프런트 계층·기능 경계 (docs/frontend-architecture.md 6절). 기능 목록은 src/features 폴더에서 읽는다.
const FEATURES = fs
  .readdirSync(path.join(__dirname, 'src/features'), { withFileTypes: true })
  .filter((d) => d.isDirectory())
  .map((d) => d.name)
// features·app 밖의 src 최상위 항목 전부(native·shared·main.tsx와 앞으로 생길 파일·폴더). 새 항목도 자동으로 규칙 ① 대상
const OUTSIDE_FEATURES = fs
  .readdirSync(path.join(__dirname, 'src'))
  .filter((name) => name !== 'features' && name !== 'app')
  .map((name) => `./src/${name}`)
// app이 전역 순서를 정해 import하는 기능 CSS(지금은 어느 화면에서든 쓰이는 결제 스타일). 컴포넌트로 옮기면 지운다(문서 9절)
const APP_GLOBAL_CSS = { payment: ['./Pricing.css', './PremiumModal.css', './Payment.css'] }

const boundaryZones = [
  // ① 기능 내부 파일은 그 기능 안에서만. 밖(다른 기능·app·native·shared)에서는 index.ts로만
  ...FEATURES.flatMap((name) => [
    {
      target: [...OUTSIDE_FEATURES, ...FEATURES.filter((other) => other !== name).map((other) => `./src/features/${other}`)],
      from: `./src/features/${name}`,
      except: ['./index.ts'],
      message: `다른 곳에서는 features/${name}의 공개 API(index.ts)만 import한다.`,
    },
    {
      target: './src/app',
      from: `./src/features/${name}`,
      except: ['./index.ts', ...(APP_GLOBAL_CSS[name] ?? [])],
      message: `app도 features/${name}의 공개 API(index.ts)만 import한다.`,
    },
  ]),
  // ② shared는 맨 아래 — 기능·엔진 브릿지·app을 모른다
  {
    target: './src/shared',
    from: ['./src/features', './src/native', './src/app'],
    message: 'shared는 features·native·app에 의존하지 않는다.',
  },
  // ③ native(엔진 브릿지)는 기능을 모른다
  {
    target: './src/native',
    from: ['./src/features', './src/app'],
    message: 'native는 features·app에 의존하지 않는다.',
  },
  // ④ 기능은 app(조립 계층)을 모른다
  {
    target: './src/features',
    from: './src/app',
    message: 'features는 app에 의존하지 않는다.',
  },
]

module.exports = {
  root: true,
  env: { browser: true, es2020: true },
  extends: [
    'eslint:recommended',
    'plugin:@typescript-eslint/recommended',
    'plugin:react-hooks/recommended',
  ],
  ignorePatterns: ['dist', '.eslintrc.cjs'],
  parser: '@typescript-eslint/parser',
  plugins: ['react-refresh', 'import'],
  settings: {
    'import/extensions': ['.ts', '.tsx', '.js'],
    // 플러그인이 리포 루트 node_modules로 올라가 있어도 client의 TS 파서를 찾게 절대 경로로 준다(이름만 주면 순환 검사가 TS 파일을 못 읽어 조용히 통과한다)
    'import/parsers': { [require.resolve('@typescript-eslint/parser')]: ['.ts', '.tsx'] },
    'import/resolver': { node: { extensions: ['.ts', '.tsx', '.js', '.mjs'] } },
  },
  rules: {
    'react-refresh/only-export-components': [
      'warn',
      { allowConstantExport: true },
    ],
  },
  overrides: [
    {
      files: ['src/**/*.ts', 'src/**/*.tsx'],
      rules: {
        'import/no-restricted-paths': ['error', { zones: boundaryZones }],
        // ⑤ 순환 금지(기능 간 index 순환 포함). 외부 패키지는 보지 않는다
        'import/no-cycle': ['error', { ignoreExternal: true }],
      },
    },
  ],
}
