# editchain

**author**: ambientlight (human written)  
**seed for**: https://chatgpt.com/share/6a4feb3a-75a8-83e8-9d39-94bb942fad58  
**next**: [seed.gpt55pro.md](./drs/seed.gpt55pro.md)

This effort is focused on building a normalized agent history representation that allows to blend agent session transcript with human edits. 
This edit chain (history) will replace the claude code session history and will be used as a backbone for idle cli orchestrator.

There are 3 main experimental ideas here to wrestle around with:

1. Editchain itself is a CRDT and so we can assume eventual consistency for cases when subagents run in isolated environments or if subagents are like IoT devices.
2. Editchain needs to support ultra-fast lookup, search, filtering for agents that run on it and can progressively reason over its ongoing work.
3. Editchain needs to solve compacts by tethering with sglang it a neat way that allows to keep the running context without invalidating the cache. This can potentially mean hooking up into DSv4 attention indexer or smt more exotic that I have no background directly.

Once we have the edit chain in place the full agent + human edit history can serve as POW (Proof-of-work) and can be submitted alongside the Github PR. We will write the github plugin that will hook with PR and analyze the edit chain. Our VSCode extension will be used to capture human edit events and add it our edit chain. We will be able to answer questions on how was code refined - agent -> human or other way round that should help us to measure the heatmap of sorts of hot and cold code -> cold that is super infrequently or like absolutely never touched by human editors and hot that is being acted upon and fixed regularly. 

The editchain will be used alongside the git history to show the full agent - human work progression for extended grounding. This is paramaunt so that search and filtering will work great on data structure itself, I don't want to speculate if vectorize the edit chain as we go but I need to understand with respect to the data structure itself - so that the data structure properties drive those choinces that simplies the retrieval job. 

Editchain itself will be bulky just as the claude code history is but there is quick way to read the shorter edits-only form. Claude code history need to also be able to convert into the edit chain. Maybe not the other way back unless cc has the stable spec for its history so we don't need to keep patching it as it breaks if its taxonomy is private. 

During this effort we will also build the edit chain viewer akin to https://github.com/jhlee0409/claude-code-history-viewer but more stripped down. 