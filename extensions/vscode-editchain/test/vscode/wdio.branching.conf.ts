import { config as live } from './wdio.live.conf';

export const config = { ...live, specs: ['./history-branching.e2e.ts'] };
