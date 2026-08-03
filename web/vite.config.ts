import { defineConfig } from "vite";

export default defineConfig({
  // No server component ever (DESIGN.md §4), so this builds to plain static
  // files. Relative base so it works from any subdirectory, or off a file path.
  base: "./",
  build: {
    target: "es2022",
    rollupOptions: {
      // Three entry points: the landing page, the sender, the receiver.
      input: {
        index: "index.html",
        skin: "skin.html",
        eye: "eye.html",
      },
    },
  },
});
