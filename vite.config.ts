import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": "/src" },
  },
  build: {
    target: "safari13",
    minify: "esbuild",
    outDir: "dist",
    emptyOutDir: true,
  },
});
