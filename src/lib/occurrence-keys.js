/**
 * Keys for the rows of a track list that hold across snapshots of it.
 *
 * A row's identity is what it plays (its URI, or its id where it has none),
 * when it was added and the context it plays in; the nth row with an identity
 * is occurrence n. So a list that comes back reordered, refreshed or with
 * another row gone keys every row it kept the same, and keyed rendering,
 * focus and drags follow the row rather than its index. Exact duplicates are
 * told apart by order alone, which a new snapshot cannot prove: a reader that
 * holds on to one key also holds on to how many rows shared its identity
 * (`occurrences(keys).multiplicity`), and lets go when that number moves.
 */
const SEPARATOR = "\u0000";

export function occurrenceKeys(rows) {
  const counts = new Map();
  return rows.map((row) => {
    const identity = `${row.uri ?? row.id}${SEPARATOR}${row.added_at ?? ""}${SEPARATOR}${row.context ?? ""}`;
    const occurrence = counts.get(identity) ?? 0;
    counts.set(identity, occurrence + 1);
    return `${identity}${SEPARATOR}${occurrence}`;
  });
}

/** A key without its occurrence: the identity it shares with its duplicates. */
export function occurrenceIdentity(key) {
  return key.slice(0, key.lastIndexOf(SEPARATOR));
}

/**
 * Where each key sits in `keys`, and how many keys share its identity.
 *
 * Only a focused or dragged row ever asks, so the index is built on the first
 * question rather than on every change of a list that may be 10,000 rows long.
 */
export function occurrences(keys) {
  let indices = null;
  let counts = null;
  const build = () => {
    indices = new Map();
    counts = new Map();
    for (let i = 0; i < keys.length; i++) {
      indices.set(keys[i], i);
      const identity = occurrenceIdentity(keys[i]);
      counts.set(identity, (counts.get(identity) ?? 0) + 1);
    }
  };
  return {
    keys,
    indexOf(key) {
      if (!indices) build();
      return indices.get(key) ?? -1;
    },
    multiplicity(key) {
      if (!counts) build();
      return counts.get(occurrenceIdentity(key)) ?? 0;
    },
  };
}
