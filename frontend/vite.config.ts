import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// AIVX 前端（对齐 AIGX frontend）：
// - 构建产物输出到 ../static（后端单二进制同目录托管）
// - 开发代理 /api → 后端 18443
export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
    globals: true,
    // 本机/CI 全局 NODE_ENV=production 会让 vitest 用生产 React（无 act）。
    // 强制 test 环境（React development build 才有 act）。
    env: { NODE_ENV: 'test' },
  },
  resolve: {
    extensions: ['.mjs', '.mts', '.ts', '.tsx', '.js', '.jsx', '.json'],
  },
  server: {
    port: 3000,
    proxy: {
      '/api': 'http://127.0.0.1:18443',
    },
  },
  build: {
    outDir: '../static',
    emptyOutDir: true,
    rollupOptions: {
      output: {
        manualChunks: {
          'vendor-react': ['react', 'react-dom', 'react-router-dom'],
        },
      },
    },
  },
});