# Models used

Before r1, Claude Code v2.1.105 used local inference on [4× RTX6000 Blackwell Max-Q](https://forum.level1techs.com/t/llm-inference-workstation-4x-rtx6000-blackwell-pro-max-q-384gb-vram-threadripper-pro-7985wx-wrx90e-sage-se-512gb-ram-1700w/252181) via [sglang-sm120-mxfp4](https://hub.docker.com/r/ambientlight/sglang-sm120-mxfp4):

- original [deepseek-v4-flash](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash) @ low-temp(temp 0.2 / top_p 0.95 / freq_p 0.1) — until [4a24241](https://github.com/idleai/editchain/commit/4a2424191bd90be3d1e669273a50b303f57d6b67)
- [deepseek-v4-flash-0731](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash-0731) @ low-temp(temp 0.2 / top_p 0.95 / freq_p 0.1) from [4a24241](https://github.com/idleai/editchain/commit/4a2424191bd90be3d1e669273a50b303f57d6b67) to [a8b0a0b](https://github.com/idleai/editchain/commit/a8b0a0b8996821919ad7e2a9c78c8a6373ddcda2)
- [deepseek-v4-flash-0731](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash-0731) @ default — from [a8b0a0b](https://github.com/idleai/editchain/commit/a8b0a0b8996821919ad7e2a9c78c8a6373ddcda2) to [faeadd9](https://github.com/idleai/editchain/commit/faeadd961002dc6956a9f46dcf7bcab49eef39a3)

From the r1 implementation commit [faeadd9](https://github.com/idleai/editchain/commit/faeadd961002dc6956a9f46dcf7bcab49eef39a3), the workflow is hybrid: `gpt-5.6-sol` at max reasoning drives the main loop, with local `deepseek-v4-flash-0731` (`dsv4-flash`) subagents.

Source task prompts live in [quests](./quests/). Hybrid runs use the Codex main loop for coordination and local dsv4-flash subagents for delegated work, with outcomes captured in `.result.md`. Raw Claude Code and Codex trajectories live at [editchain-sessions-raw](https://github.com/idleai/editchain-sessions-raw).
