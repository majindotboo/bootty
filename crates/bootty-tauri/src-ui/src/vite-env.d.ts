/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_TERMINAL_BACKEND?: "site";
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}