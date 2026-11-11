import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { svelteTesting } from "@testing-library/svelte/vite";

export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2022", outDir: "dist", emptyOutDir: true },
  // Pure tests run in node; component tests opt into jsdom per file.
  test: { include: ["src/**/*.test.ts"], environment: "node" },
});
