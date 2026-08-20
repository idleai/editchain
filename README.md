# editchain

CRDT-based editchain constructed from cc history with a minimal raw **VS Code viewer** for debugging. WIP / experiment.

## VS Code extension

Under [`extensions/vscode-editchain/`](./extensions/vscode-editchain/)

## Quick start (CLI)

```sh
cargo build
cargo run --bin editchain -- init my-chain
cargo run --bin editchain -- import \
  --sessions-dir /path/to/cc-sessions --workspace /path/to/repo --chain ./outputs/cc-chain
cargo run --bin editchain -- search ./outputs/cc-chain "query" --mode hybrid --top 10
cargo run --bin editchain -- retrieve ./outputs/cc-chain --op "<op-id>"
```

## Models used

Claude Code v2.1.105, local inference on [4× RTX6000 Blackwell Max-Q](https://forum.level1techs.com/t/llm-inference-workstation-4x-rtx6000-blackwell-pro-max-q-384gb-vram-threadripper-pro-7985wx-wrx90e-sage-se-512gb-ram-1700w/252181) via [sglang-sm120-mxfp4](https://hub.docker.com/r/ambientlight/sglang-sm120-mxfp4):
- original [deepseek-v4-flash](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash) @ low-temp(temp 0.2 / top_p 0.95 / freq_p 0.1) — until [4a24241](https://github.com/idleai/editchain/commit/4a2424191bd90be3d1e669273a50b303f57d6b67)
- [deepseek-v4-flash-0731](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash-0731) @ low-temp(temp 0.2 / top_p 0.95 / freq_p 0.1) from [4a24241](https://github.com/idleai/editchain/commit/4a2424191bd90be3d1e669273a50b303f57d6b67) to [a8b0a0b](https://github.com/idleai/editchain/commit/a8b0a0b8996821919ad7e2a9c78c8a6373ddcda2)
- [deepseek-v4-flash-0731](https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash-0731) @ default — from [a8b0a0b](https://github.com/idleai/editchain/commit/a8b0a0b8996821919ad7e2a9c78c8a6373ddcda2)

Source task prompts at [quests](./quests/) hit chatgpt/codex for base grounding and bounce `.result.md` back that get picked up by local dsv4-flash cc. Raw cc trajectories at [editchain-sessions-raw](https://github.com/idleai/editchain-sessions-raw)