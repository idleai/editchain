/** Typed bridge to app-core state. Node owns I/O; Rust owns join/retry/status policy. */
export interface JoinState {
  readonly generation: number;
  readonly enabled: boolean;
  enable(generation: number): boolean;
  is_current(generation: number): boolean;
  retire(): void;
}

export interface ConnectionState {
  readonly generation: number;
  readonly status: string;
  readonly retry_delay_ms: number;
  begin(): number;
  stop(): void;
  waiting(generation: number): void;
  expired(generation: number): void;
  ready(generation: number): void;
  authenticating(generation: number): void;
  progress(generation: number, json: string): void;
}

type Bindings = {
  SharedJoin: new () => JoinState;
  SharedConnection: new () => ConnectionState;
};

function bindings(): Bindings {
  // This generated Node/WASM adapter calls the same app-core portable state as
  // Crux. It contains no credentials, VS Code APIs or transport implementation.
  return require('../../media/client-state/pkg/editchain_client_state.js') as Bindings;
}

export function joinState(): JoinState { return new (bindings().SharedJoin)(); }
export function connectionState(): ConnectionState { return new (bindings().SharedConnection)(); }
