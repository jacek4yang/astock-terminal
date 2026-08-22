import "@testing-library/jest-dom/vitest";

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
