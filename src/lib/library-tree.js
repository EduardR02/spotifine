/**
 * Edits to the library's folder tree — `libraryState.tree`, the one model of
 * it — made the way the rootlist edits land, so the rail can show an edit
 * the moment it is asked for and the shell's own copy (which applies the
 * same edit when the write succeeds) agrees with it.
 *
 * Nodes are `{ kind: "folder", id, name, children, last_activity? }` and
 * `{ kind: "playlist", id }`; an item names a node as `{ kind, id }`. A
 * playlist the tree does not name sits at the top level. Every function
 * returns a new tree and leaves the one it was given untouched — so a write
 * that fails can put the old one back — and reuses every node off the edited
 * path.
 */

const is = (node, item) => node?.kind === item?.kind && node?.id === item?.id;

export function findFolder(tree, id) {
  for (const node of tree ?? []) {
    if (node.kind !== "folder") continue;
    if (node.id === id) return node;
    const found = findFolder(node.children, id);
    if (found) return found;
  }
  return null;
}

/** The id of the folder holding `item`; null at (or absent from) the top level. */
export function parentOf(tree, item, parent = null) {
  for (const node of tree ?? []) {
    if (is(node, item)) return parent;
    if (node.kind === "folder") {
      const found = parentOf(node.children, item, node.id);
      if (found !== undefined) return found;
    }
  }
  return parent === null ? null : undefined;
}

/** Whether `id` is `folder` or a folder somewhere inside it. */
function holds(folder, id) {
  return folder?.id === id || !!findFolder(folder?.children, id);
}

/** The top-level folder node that holds `item` at any depth, if one does. */
function topLevelFolderOf(tree, item) {
  return tree.find((node) => node.kind === "folder" && !is(node, item) && contains(node, item)) ?? null;
}

function contains(folder, item) {
  return (folder.children ?? []).some((node) => is(node, item) || (node.kind === "folder" && contains(node, item)));
}

function take(nodes, item) {
  const index = nodes.findIndex((node) => is(node, item));
  if (index >= 0) return [nodes.filter((_, at) => at !== index), nodes[index]];
  for (let at = 0; at < nodes.length; at++) {
    const node = nodes[at];
    if (node.kind !== "folder") continue;
    const [children, taken] = take(node.children ?? [], item);
    if (taken) return [nodes.map((other, i) => (i === at ? { ...node, children } : other)), taken];
  }
  return [nodes, null];
}

function mapFolder(nodes, id, edit) {
  let changed = false;
  const next = nodes.map((node) => {
    if (changed || node.kind !== "folder") return node;
    if (node.id === id) {
      changed = true;
      return edit(node);
    }
    const children = mapFolder(node.children ?? [], id, edit);
    if (children === node.children) return node;
    changed = true;
    return { ...node, children };
  });
  return changed ? next : nodes;
}

/** `node` first in folder `parent`, or first at the top level. */
export function insertFirst(tree, parent, node) {
  const nodes = tree ?? [];
  if (parent && findFolder(nodes, parent)) {
    return mapFolder(nodes, parent, (folder) => ({ ...folder, children: [node, ...(folder.children ?? [])] }));
  }
  return [node, ...nodes];
}

/**
 * Moves a playlist or a folder to the top of `folder`, or — null — out of
 * every folder, to just after the top-level folder it was in: the place the
 * engine's rootlist MOV puts it, so the tree reads the same before and after
 * the account answers. Null when there is nothing to do (already there) or
 * it cannot be done: a folder into itself or into a folder of its own.
 */
export function moveItem(tree, item, folder) {
  const nodes = tree ?? [];
  const target = folder ?? null;
  if (target && !findFolder(nodes, target)) return null;
  if (parentOf(nodes, item) === target) return null;
  if (item.kind === "folder" && target && holds(findFolder(nodes, item.id), target)) return null;
  const outermost = target ? null : topLevelFolderOf(nodes, item);
  const [rest, taken] = take(nodes, item);
  const node = taken ?? (item.kind === "playlist" ? { kind: "playlist", id: item.id } : null);
  if (!node) return null;
  if (outermost) {
    const at = rest.findIndex((other) => is(other, outermost)) + 1;
    return [...rest.slice(0, at), node, ...rest.slice(at)];
  }
  return insertFirst(rest, target, node);
}

export function renameFolder(tree, id, name) {
  return mapFolder(tree ?? [], id, (folder) => ({ ...folder, name }));
}

/** Every playlist id at or under a node. */
export function playlistIds(node, out = []) {
  if (node?.kind === "playlist") out.push(node.id);
  else for (const child of node?.children ?? []) playlistIds(child, out);
  return out;
}

/** What a folder holds, for saying so before it is deleted. */
export function folderContents(folder) {
  let playlists = 0;
  let folders = 0;
  const walk = (nodes) => {
    for (const node of nodes ?? []) {
      if (node.kind === "folder") {
        folders += 1;
        walk(node.children);
      } else playlists += 1;
    }
  };
  walk(folder?.children);
  return { playlists, folders };
}

/** The folder and everything in it gone, with the playlists that went too. */
export function removeFolder(tree, id) {
  const [rest, taken] = take(tree ?? [], { kind: "folder", id });
  return { tree: rest, playlistIds: taken ? playlistIds(taken) : [] };
}

export function removePlaylist(tree, id) {
  return take(tree ?? [], { kind: "playlist", id })[0];
}

/**
 * The folders `item` can move into, in tree order with their depth: every
 * folder but the one it is in, and — for a folder — never itself or any
 * folder inside it.
 */
export function folderTargets(tree, item) {
  const parent = parentOf(tree, item);
  const out = [];
  const walk = (nodes, depth) => {
    for (const node of nodes ?? []) {
      if (node.kind !== "folder") continue;
      if (item.kind === "folder" && node.id === item.id) continue;
      if (node.id !== parent) out.push({ id: node.id, name: node.name, depth });
      walk(node.children, depth + 1);
    }
  };
  walk(tree, 0);
  return out;
}
