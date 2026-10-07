/** A panel owns its requests and media arrivals only for its current track. */
export function createCanvasOwner(load, changed) {
  let generation = 0;
  let key = "";
  let asked = "";
  let accepted = "";
  let allowed = false;
  let disposed = false;
  let url = "";
  let ready = false;
  let waiting = false;

  function publish(resetPreference = false) {
    changed({ url, ready, waiting, accepted: accepted === key && !!key, resetPreference });
  }
  function drop() {
    url = "";
    ready = waiting = false;
    accepted = "";
    publish(true);
  }
  function update(nextKey, enabled, visible) {
    if (disposed) return;
    if (nextKey !== key || enabled !== allowed) {
      generation++;
      key = nextKey;
      allowed = enabled;
      asked = accepted = "";
      ready = waiting = false;
      if (!enabled) url = "";
      publish(true);
    }
    if (!allowed || !key || !visible || asked === key) return;
    asked = key;
    const ticket = ++generation;
    const requestedKey = key;
    waiting = true;
    publish();
    Promise.resolve().then(() => {
      if (!disposed && ticket === generation) return load(requestedKey);
    }).then((canvas) => {
      if (disposed || ticket !== generation) return;
      if (!canvas?.url) return drop();
      url = canvas.url;
      accepted = key;
      publish();
    }).catch(() => {
      if (!disposed && ticket === generation) drop();
    });
  }
  function arrive(source) {
    if (disposed || accepted !== key || !accepted || source !== url || ready) return;
    ready = true;
    waiting = false;
    publish();
  }
  function fail(source) {
    if (!disposed && accepted === key && accepted && source === url) drop();
  }
  function dispose() {
    disposed = true;
    generation++;
  }
  return { update, arrive, fail, dispose };
}
