/**
 * Bounded colour-sampling work shared across cards and mosaic cells.
 * Identical URLs share a decode; unrelated reads wait their turn.
 */

/**
 * A memo and a concurrency-limited queue in front of an async reader.
 *
 * `read` is what one value costs to work on — decoding a cover to read its
 * colour, here. Two of those run per image otherwise: `coverTone` keys its own
 * cache by the whole pool of urls, which is right, because four cells are one
 * answer — but it means one file answers to several keys. The memo is keyed
 * per url, so the second ask shares the first one's promise instead of
 * decoding the same picture again.
 *
 * The queue is the other half. A library page mounts two hundred cards and
 * asks for every one of their colours at once; started together, the covers
 * nobody is looking at take the browser's decode queue ahead of the ones on
 * screen. `limit` reads run at a time and the rest wait their turn in the
 * order they were asked for, so the top of a list is served first.
 *
 * @param read  `(value) => Promise` — the work for one value
 * @param max   results the memo keeps, oldest evicted first
 * @param limit reads allowed to run at once
 * @returns `(key, value) => Promise`
 */
export function boundedReads(read, { max, limit }) {
  /** key → the promise, so a caller arriving mid-read shares it. */
  const memo = new Map();
  const queue = [];
  let reading = 0;

  const pump = () => {
    while (reading < limit && queue.length) {
      const next = queue.shift();
      reading += 1;
      /* Wrapped in an async function so a reader that throws before it
         returns a promise rejects its own caller instead of escaping into
         this loop and stranding everything queued behind it. */
      (async () => read(next.value))()
        .then(next.resolve, next.reject)
        .finally(() => {
          reading -= 1;
          pump();
        });
    }
  };

  return (key, value) => {
    const hit = memo.get(key);
    if (hit) return hit;
    const pending = new Promise((resolve, reject) => {
      queue.push({ value, resolve, reject });
      pump();
    });
    memo.set(key, pending);
    // A load error is not a sample. A later caller may try again; never
    // remove a newer read if this one was evicted before it finished.
    pending.catch(() => {
      if (memo.get(key) === pending) memo.delete(key);
    });
    if (memo.size > max) memo.delete(memo.keys().next().value);
    return pending;
  };
}

