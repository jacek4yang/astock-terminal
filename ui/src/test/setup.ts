import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Unmount rendered components between tests.
//
// React Testing Library registers this itself only when the test framework's
// globals are enabled; this project runs Vitest without `globals`, so nothing was
// unmounting. Every rendered component stayed in the document, and a file that
// rendered twice made the second test query a DOM containing both copies —
// `Found multiple elements with the text: …`. Whether it failed depended on how far
// the first test's async work had progressed, which is exactly the kind of flake
// that makes a CI gate untrustworthy.
afterEach(cleanup);

// Node 26 exposes an experimental, unconfigured global localStorage getter
// which can shadow jsdom's storage. Use a deterministic in-memory browser
// storage so persisted Zustand stores are exercised in tests.
const values = new Map<string, string>();
const memoryStorage: Storage = {
  get length() {
    return values.size;
  },
  clear: () => values.clear(),
  getItem: (key) => values.get(key) ?? null,
  key: (index) => [...values.keys()][index] ?? null,
  removeItem: (key) => {
    values.delete(key);
  },
  setItem: (key, value) => {
    values.set(key, value);
  },
};

Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: memoryStorage,
});
Object.defineProperty(window, "localStorage", {
  configurable: true,
  value: memoryStorage,
});
