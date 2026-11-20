import fs from 'node:fs'
import path from 'node:path'
import js from '@eslint/js'
import { defineConfig, globalIgnores } from 'eslint/config'
import { createNodeResolver, importX } from 'eslint-plugin-import-x'
import reactHooks from 'eslint-plugin-react-hooks'
import { reactRefresh } from 'eslint-plugin-react-refresh'
import globals from 'globals'
import tseslint from 'typescript-eslint'

const dirname = import.meta.dirname

// 프런트 계층·기능 경계 (docs/frontend-architecture.md 6절). 기능 목록은 src/features 폴더에서 읽는다.
const FEATURES = fs
  .readdirSync(path.join(dirname, 'src/features'), { withFileTypes: true })
  .filter((d) => d.isDirectory())
  .map((d) => d.name)
// features·app 밖의 src 최상위 항목 전부(native·shared·main.tsx와 앞으로 생길 파일·폴더). 새 항목도 자동으로 규칙 ① 대상
const OUTSIDE_FEATURES = fs
  .readdirSync(path.join(dirname, 'src'))
  .filter((name) => name !== 'features' && name !== 'app')
  .map((name) => `./src/${name}`)
// app이 전역 순서를 정해 import하는 기능 CSS(결제 CSS 중 결제 화면 밖에서도 매칭되는 규칙, P3-2 ③-2). 화면별 CSS 정리 때 지운다(문서 9절)
const APP_GLOBAL_CSS = {
  payment: ['./paymentGlobal.css'],
  profile: ['./profileGlobal.css'],
  'demo-karaoke': ['./karaokeGlobal.css'],
}

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

// eslint-plugin-react-hooks 7 권장 설정에 새로 들어온 React Compiler 규칙 중, 기존 코드에 이미 위반이 있는 것.
// 고치려면 상태·ref 흐름을 바꾸는 로직 수정이 필요해 이번 업그레이드(P3-20b)에서는 손대지 않는다.
// 규칙을 통째로 끄지 않고 "지금 위반이 있는 파일"만 명시적으로 끈다 — 나머지 파일과 새 파일에는 error로 걸린다.
// 후속: 파일을 고치면 목록에서 지운다(목록이 비면 항목 삭제).
const REACT_COMPILER_LEGACY_FILES = {
  // effect 안에서 동기 setState(연쇄 렌더). 파생 상태·key 리셋·이벤트 핸들러로 옮기는 수정이 필요
  'react-hooks/set-state-in-effect': [
    'src/features/auth/AuthContext.tsx',
    'src/features/community/EditPost.tsx',
    'src/features/demo-karaoke/MusicRoom.tsx',
    'src/features/payment/PaymentSuccess.tsx',
    'src/features/profile/useProfileForm.ts',
    'src/features/room/components/MetronomePanel.tsx',
    'src/features/room/components/NativeRecordingControl.tsx',
    'src/features/settings/AudioSettingsContext.tsx',
    'src/features/settings/EchoMeasure.tsx',
    'src/features/settings/hooks/useLivePeerStats.ts',
  ],
  // 렌더 중 ref.current 읽기·쓰기("최신 값 ref" 패턴). useEffectEvent·effect 안 갱신으로 옮기는 수정이 필요
  'react-hooks/refs': [
    'src/features/payment/PaymentSuccess.tsx',
    'src/features/room/hooks/useRoomPresence.ts',
    'src/features/room/state/RoomProvider.tsx',
    'src/features/room/state/useRecording.ts',
    'src/features/room/state/useRoomSelector.ts',
    'src/features/room/state/useSignaling.ts',
    'src/features/room/state/useViewerAudio.ts',
    'src/features/room/state/useWebAudioMixer.ts',
    'src/features/settings/AudioSettingsContext.tsx',
    'src/native/useNativeSession.ts',
  ],
  // 렌더 중 Math.random·Date.now 호출. 값 생성 시점을 state 초기화·effect로 옮기는 수정이 필요
  'react-hooks/purity': ['src/features/demo-recording/Recording.tsx', 'src/features/payment/PaymentSuccess.tsx'],
  // state로 든 SVG 요소의 style을 effect에서 직접 바꾼다. ref로 옮기는 수정이 필요
  'react-hooks/immutability': ['src/features/demo-karaoke/ScoreDisplay.tsx'],
}

export default defineConfig([
  // dist·node_modules 밖 client의 TS/TSX 전부가 대상(예전 `eslint . --ext ts,tsx`와 같은 범위)
  globalIgnores(['dist', 'src-tauri']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [js.configs.recommended, tseslint.configs.recommended, reactHooks.configs.flat.recommended],
    languageOptions: {
      ecmaVersion: 2020,
      globals: { ...globals.browser, ...globals.es2020 },
    },
    plugins: {
      'react-refresh': reactRefresh.plugin,
      'import-x': importX,
    },
    settings: {
      'import-x/extensions': ['.ts', '.tsx', '.js'],
      'import-x/resolver-next': [createNodeResolver({ extensions: ['.ts', '.tsx', '.js', '.mjs'] })],
    },
    rules: {
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
      // eslint 8 eslint:recommended에 있다가 9·10 권장에서 빠졌거나 typescript-eslint가 끄는 규칙. 엄격함을 낮추지 않게 그대로 켠다
      'no-class-assign': 'error',
      'no-with': 'error',
      // v8과 같은 동작(블록 안 함수 선언도 금지)
      'no-inner-declarations': ['error', 'functions', { blockScopedFunctions: 'disallow' }],
      // 서식 규칙이라 폐기 예정(@stylistic로 이전)이지만 eslint 10에도 남아 있어 v8과 같게 유지한다
      'no-extra-semi': 'error',
      'no-mixed-spaces-and-tabs': 'error',
    },
  },
  {
    // 시험 하네스 컴포넌트가 훅 결과를 바깥 변수에 담아 단언하는 패턴(렌더되는 앱 코드가 아님)
    files: ['**/*.test.{ts,tsx}'],
    rules: {
      'react-hooks/globals': 'off',
      'react-hooks/immutability': 'off',
    },
  },
  ...Object.entries(REACT_COMPILER_LEGACY_FILES).map(([rule, files]) => ({
    files,
    rules: { [rule]: 'off' },
  })),
  {
    // flat config는 .js·.mjs도 기본 대상이다(eslint.config.js, e2e/pick-port.mjs). 예전엔 --ext로 빠졌던 파일 — 기본 권장 규칙만 건다
    files: ['**/*.{js,mjs}'],
    extends: [js.configs.recommended],
    languageOptions: { globals: { ...globals.node } },
  },
  {
    files: ['src/**/*.ts', 'src/**/*.tsx'],
    rules: {
      'import-x/no-restricted-paths': ['error', { zones: boundaryZones }],
      // ⑤ 순환 금지(기능 간 index 순환 포함). 외부 패키지는 보지 않는다
      'import-x/no-cycle': ['error', { ignoreExternal: true }],
    },
  },
])
