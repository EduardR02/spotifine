/* The demo keeps nothing on a visitor's computer: the app's localStorage
   (panel width, sort orders, pins) lives in memory for the visit, and every
   visit opens the same way, the Now playing panel open. Imported first, so
   it is in place before any app module reads storage at load. */
const memory = new Map([["sr.now-playing-open", "1"]]);

const storage = {
  get length() {
    return memory.size;
  },
  key: (index) => [...memory.keys()][index] ?? null,
  getItem: (key) => memory.get(String(key)) ?? null,
  setItem: (key, value) => void memory.set(String(key), String(value)),
  removeItem: (key) => void memory.delete(String(key)),
  clear: () => memory.clear(),
};

Object.defineProperty(window, "localStorage", { value: storage, configurable: true });
