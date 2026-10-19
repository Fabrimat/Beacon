/// <reference types="vitest/config" />
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "chrome105", sourcemap: false },
  test: { environment: "jsdom", include: ["test/**/*.test.tsx"] },
});
