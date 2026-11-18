# Build stage — 베이스 이미지는 태그+다이제스트로 고정 (갱신 방법: relay/README.md)
FROM node:22.23.5-alpine3.24@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a AS builder

WORKDIR /app

# Copy package files
COPY package*.json ./

# Install dependencies
RUN npm install

# Copy source code
COPY tsconfig.json ./
COPY src ./src

# Build TypeScript
RUN npm run build

# Production stage
FROM node:22.23.5-alpine3.24@sha256:1314fb30ec37592d5845e5a160d522c0e680972aa534549940b8d73bde88481a

WORKDIR /app
ENV NODE_ENV=production

# 스템 압축(WAV → Opus/Ogg)용 ffmpeg (alpine 패키지는 libopus 포함)
RUN apk add --no-cache ffmpeg

# Copy package files and install production dependencies only
COPY package*.json ./
RUN npm install --omit=dev

# Copy built files from builder stage
COPY --from=builder /app/dist ./dist

# Expose port
EXPOSE 8080

# Start the server
CMD ["node", "dist/index.js"]
