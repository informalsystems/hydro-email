import { defineConfig } from 'vite';

export default defineConfig({
  // The repository .env belongs to the server, never to Vite or its browser bundle.
  envDir: false,
  envPrefix: 'HYDRO_PUBLIC_UNUSED_',
});
