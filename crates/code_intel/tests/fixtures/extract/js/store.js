import { emit } from "./events.js";

const LIMIT = 100;
let instances = 0;

function clamp(value, low, high) {
  return Math.min(Math.max(value, low), high);
}

export class Store {
  #items = [];

  constructor(name) {
    this.name = name;
    instances += 1;
  }

  add(item, ...rest) {
    const next = [item, ...rest];
    const keep = (x) => x != null;
    this.#items.push(...next.filter(keep));
    this.#trim();
    emit("add", this.name);
  }

  #trim() {
    const size = clamp(this.#items.length, 0, LIMIT);
    this.#items.length = size;
  }

  *entries() {
    yield* this.#items.entries();
  }
}

export const createStore = (name) => new Store(name);

const api = {
  reset() {
    instances = 0;
  },
  count: () => instances,
};

function* ids(start = 0) {
  let n = start;
  while (true) yield n++;
}

export { api, ids as idSequence };
export default createStore;
