// Service bridge: drives the REAL Rust editchain-vscode-service over framed
// stdio, so the harness renders actual chain data instead of fixtures.
//
// Loaded in the harness page BEFORE the renderer bootstrap (replacing
// fixtureBridge.js). It defines a global `vscode` object whose postMessage
// forwards requests to the service and dispatches responses back as
// `{ id, body }` message events.
//
// The service is spawned by the harness host via a small Node helper that
// exposes a global `__editchainService` with send(body) -> Promise. This keeps
// the browser page free of Node APIs.

(function () {
  'use strict';

  let persistedState = undefined;
  let reqId = 0;

  // The Node host injects this before the renderer runs. Resolve lazily so
  // the bridge can be defined before the shim is wired.
  function svc() {
    return window.__editchainService;
  }

  function respond(id, body) {
    window.dispatchEvent(new MessageEvent('message', { data: { id, body } }));
  }

  // Normalize a transport/startup failure to the service's { Error: string }
  // envelope so the renderer treats it like any other service error.
  function errMsg(err) {
    return String(err && err.message || err);
  }

  window.vscode = {
    postMessage(msg) {
      if (msg && msg.body !== undefined) {
        // The renderer tags every request with a client-generated id; fall
        // back to assigning one for older callers.
        const id = typeof msg.id === 'number' ? msg.id : (++reqId);
        svc().send(msg.body).then((body) => respond(id, body))
          .catch((err) => respond(id, { Error: errMsg(err) }));
      }
      // openJson / log messages are no-ops in the harness.
    },
    getState() {
      return persistedState;
    },
    setState(state) {
      persistedState = state;
    },
  };

  window.acquireVsCodeApi = function () {
    return window.vscode;
  };

  // Emulate the extension host startup handshake: Open then ready.
  window.__editchainStart = function () {
    // Open is unbounded (0 = no deadline): building the chain + git graph can
    // take minutes on a large workspace, so the harness must not apply the
    // bounded default used for regular requests. The Node-side client forwards
    // the timeout through the injected browser shim.
    svc().send({ Open: { workspace_path: window.__editchainWorkspace, chain_dir: window.__editchainChainDir } }, 0)
      .then((body) => {
        window.dispatchEvent(new MessageEvent('message', { data: { id: 'open', body } }));
        // Mirror the extension host: `ready` (which makes the renderer fetch
        // its first window) is only sent after a SUCCESSFUL Open — an Open
        // Error surfaces visibly and must not trigger a window fetch.
        if (body && body.Ok !== undefined && body.Ok !== null) {
          window.dispatchEvent(new MessageEvent('message', { data: { id: 'ready', body: { Ok: {} } } }));
        }
      })
      .catch((err) => {
        window.dispatchEvent(new MessageEvent('message', {
          data: { id: 'open', body: { Error: errMsg(err) } },
        }));
      });
  };
})();
