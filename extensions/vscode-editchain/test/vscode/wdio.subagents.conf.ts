import { config as live } from './wdio.live.conf';

export const config = { ...live, specs: ['./history-subagents.e2e.ts'] };
