# band-server (fly.io). 빌드 컨텍스트는 리포 루트다(P3-26) — 루트 package-lock.json으로 의존 버전을 고정한다.
#   로컬: (루트에서) docker build -f server/Dockerfile -t band-server .
#   배포: (루트에서) flyctl deploy . --config server/fly.toml --dockerfile server/Dockerfile --remote-only
# 컨텍스트에 들어가는 파일은 루트 .dockerignore(허용 목록)가 정한다.
# server/를 컨텍스트로 빌드하면 package-lock.json이 없어 COPY에서 실패한다(lock 없이 설치되는 일을 막는다).
#
# 베이스 이미지는 태그+다이제스트로 고정 (갱신 방법: relay/README.md)
FROM node:22.23.5-alpine3.24@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a AS builder

WORKDIR /app

# 루트 매니페스트·lock + server 매니페스트만. client/는 컨텍스트에 넣지 않는다(--workspace server라 필요 없음).
COPY package.json package-lock.json ./
COPY server/package.json server/

# npm ci: lock과 package.json이 어긋나면 실패한다. server 워크스페이스의 의존만 설치(루트 의존 제외).
RUN npm ci --workspace server --include-workspace-root=false

COPY server/tsconfig.json server/
COPY server/src server/src

RUN npm run build --workspace server

# Production stage
FROM node:22.23.5-alpine3.24@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a

WORKDIR /app
ENV NODE_ENV=production

# 스템 압축(WAV → Opus/Ogg)용 ffmpeg (alpine 패키지는 libopus 포함)
RUN apk add --no-cache ffmpeg

# 같은 lock으로 prod 의존만 설치
COPY package.json package-lock.json ./
COPY server/package.json server/
RUN npm ci --workspace server --include-workspace-root=false --omit=dev && npm cache clean --force

# Copy built files from builder stage
COPY --from=builder /app/server/dist ./server/dist

# 의존은 /app/node_modules(호이스팅)와 /app/server/node_modules에서 찾는다
WORKDIR /app/server

# Expose port
EXPOSE 8080

# Start the server
CMD ["node", "dist/index.js"]
