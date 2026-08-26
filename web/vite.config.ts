import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const agentApiTarget = process.env.VITE_AGENT_API_TARGET
  || "http://127.0.0.1:3000";
const base = process.env.VITE_BASE_PATH || "/";

export default defineConfig({
  plugins: [react()],
  base,
  server: {
    allowedHosts: [".trycloudflare.com"],
    proxy: {
      "/api": agentApiTarget,
      "/health": agentApiTarget
    }
  }
});
