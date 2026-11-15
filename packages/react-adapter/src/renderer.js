// Renderer: the pinned `react-reconciler` factory instantiated with the Rendrix host
// config, plus the public mount API (`render` / `flushSync`).
//
// Scheduling: `render`/`unmount` commit synchronously (`updateContainerSync` +
// `flushSyncFromReconciler`), event-handler updates flush on a microtask (discrete lane),
// and programmatic updates outside events take the default concurrent lane — wrap those in
// `flushSync` for deterministic commits. Passive effects (`useEffect`) always flush
// asynchronously via the vendored scheduler — the headless tests use `wait` steps for those.

import createReconciler from './vendor/reconciler.js';
import { LegacyRoot } from './vendor/constants.js';
import { hostConfig } from './host.js';

const Reconciler = createReconciler(hostConfig);

const describeError = (kind, error, info) =>
  console.error(
    `rendrix-react-adapter (${kind}):`,
    error,
    info && info.componentStack ? `\n${info.componentStack}` : '',
  );

/**
 * Mount `element` into `container` (clearing it first) and return an `unmount` closure.
 * Mirrors the solid adapter's `render`: one call owns the container until unmounted.
 */
export function render(element, container) {
  if (!container || container.nodeType !== 1) {
    throw new TypeError('rendrix-react-adapter: render requires an Element container');
  }
  container.replaceChildren();
  const root = Reconciler.createContainer(
    container,
    LegacyRoot,
    null, // hydrationCallbacks
    false, // isStrictMode
    null, // concurrentUpdatesByDefaultOverride
    '', // identifierPrefix
    (error, info) => describeError('uncaught', error, info),
    (error, info) => describeError('caught', error, info),
    (error, info) => describeError('recoverable', error, info),
  );
  // Mount synchronously (legacy `ReactDOM.render` semantics): `updateContainerSync` takes
  // the sync lane and `flushSyncFromReconciler` drains it before returning, so committed
  // DOM is observable as soon as `render` returns. Passive effects still flush
  // asynchronously — tests observe them after `wait`/`settle` steps.
  Reconciler.updateContainerSync(element, root, null, null);
  Reconciler.flushSyncFromReconciler(() => {});
  let mounted = true;
  return function unmount() {
    if (!mounted) return;
    mounted = false;
    Reconciler.updateContainerSync(null, root, null, null);
    Reconciler.flushSyncFromReconciler(() => {});
  };
}

/** Flush pending work synchronously around `fn` (escape hatch for tests and event plumbing). */
export function flushSync(fn) {
  return Reconciler.flushSyncFromReconciler(fn);
}

/** Batch `fn` so its updates commit together. */
export function batchedUpdates(fn, ...args) {
  return Reconciler.batchedUpdates(fn, ...args);
}
