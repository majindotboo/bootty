/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_TERMINAL_BACKEND?: "fake";
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}