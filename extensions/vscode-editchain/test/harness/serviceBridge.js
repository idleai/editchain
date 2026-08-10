// Service bridge: drives the REAL Rust editchain-vscode-service over framed
// stdio, so the harness renders actual chain data instead of fixtures.
//
// Loaded in the harness page BEFORE media/main.js (replacing fixtureBridge.js).
// It defines a global `vscode` object whose postMessage forwards requests to
// the service and dispatches responses back as `{ id, body }` message events.
//
// The service is spawned by the harness host (index.html) via a small Node
// helper that exposes a global `__editchainService` with send(body) -> Promise.
// This keeps the browser page free of Node APIs.

(function () {
  'use strict';

  let persistedState = undefined;
  let reqId = 0;

  // The Node host injects this before main.js runs. Resolve lazily so the
  // bridge can be defined before the shim is wired.
  function svc() {
    return window.__editchainService;
  }

  function respond(id, body) {
    window.dispatchEvent(new MessageEvent('message', { data: { id, body } }));
  }

  window.vscode = {
    postMessage(msg) {
      if (msg && msg.body !== undefined && msg.id === undefined) {
        const id = ++reqId;
        svc().send(msg.body).then((body) => respond(id, body));
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
    svc().send({ Open: { workspace_path: window.__editchainWorkspace, chain_dir: window.__editchainChainDir } })
      .then((body) => {
        window.dispatchEvent(new MessageEvent('message', { data: { id: 'open', body } }));
        window.dispatchEvent(new MessageEvent('message', { data: { id: 'ready', body: { Ok: {} } } }));
      });
  };
})();