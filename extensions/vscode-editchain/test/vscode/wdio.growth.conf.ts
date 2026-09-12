import { config as live } from './wdio.live.conf';

// Same isolated VS Code environment, with deterministic +1 topology edits.
export const config = { ...live, specs: ['./history-growth.e2e.ts'] };
