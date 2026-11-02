// @ts-check
import { defineConfig } from "astro/config";
import node from "@astrojs/node";
import tailwindcss from "@tailwindcss/vite";
import icon from "astro-icon";

// The API sets `__Host-` cookies (Secure, SameSite=Lax), so the browser must see the API on the
// page's own origin. In dev Vite proxies /api; in production the gateway does the same.
// Default target is the monolith; API_MICRO=1 fans out to the four services by path prefix.
const mono = process.env.API_URL ?? "http://localhost:8181";
const micro = {
  "/api/v1/users": "http://localhost:8201",
  "/api/v1/profiles": "http://localhost:8202",
  "/api/v1/articles": "http://localhost:8203",
  "/api/v1/comments": "http://localhost:8204",
};

export default defineConfig({
  vite: {
    server: {
      proxy: process.env.API_MICRO ? micro : { "/api/v1": mono },
    },
    plugins: [tailwindcss()],
  },
  security: {
    // Astro's CSP is unsupported under `astro dev`: Vite injects styles inline and the dev
    // server still sends a bare `style-src 'self'`, which blocks every stylesheet and script.
    // Builds get an external stylesheet plus hashed scripts, so CSP is production-only.
    csp: process.env.NODE_ENV === "production",
  },
  adapter: node({
    mode: "standalone",
    staticHeaders: true,
  }),
  integrations: [icon()],
});
