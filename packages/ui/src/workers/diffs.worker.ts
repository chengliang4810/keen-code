// @pierre/diffs 未将 worker 入口列入 package.json 的 sideEffects，
// 仅有副作用的静态导入会被 Vite/Rolldown 树摇成 0 字节文件，WorkerPool 会永久等不到初始化响应。
// apps/ui/vite.config.ts 会把这个入口标记为有副作用，确保库的 message 监听器和高亮实现进入产物。
import "@pierre/diffs/worker/worker.js";
