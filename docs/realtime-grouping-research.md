# Progressive grouping for live EditChain history

Implementation decision, 2026-09-11: **group Codex work by its native thread and task/turn identity**. The chat-delimited alternatives below remain research history; they are not the live grouping rule.

The first implementation separates three retained layers:

- `LiveProjection` attaches the full thread/turn identity and rollback boundary to each independent logical item. The source generation and removal boundary prevent reused IDs from inheriting an earlier task's state.
- The native task index maintains contiguous sections in display order, member counts, an optional clipped first-user-prompt title, and explicit provider lifecycle status. A section has a stable header key. Interleaved tasks or Git rows create separate sections sharing the same task identity; history is never reordered to bring a task together. Removing a separator does not rename or merge established sections.
- The WASM viewer owns disclosure and exposure. A completed task already present at bootstrap starts folded. Other historical states and all live-created sections start open. Completion alone never collapses an open section. A new or revised item in an explicitly collapsed section is individually exposed until another manual toggle; previously hidden members stay hidden. Search reveals its exact member before requesting its content, without expanding the entire task.

`LiveBlockMeta.task_group` gives an item's section membership. A one-row header block carries `task_header` / `TaskGroupDto`; its content row carries the same summary as `HistoryRow.task_group`. Headers use a position immediately before the newest section member, but their keys do not change when that anchor advances. Headers carry passing graph connections and have no causal node or dot. Ordinary appends publish the changed item, its membership, and one small header; they do not publish a member array or a growing combined content block. Revisions of an existing item retain membership without scanning its task. Late interleaving can split and retag the affected section prefix; explicit disclosure can visit section members. Those are exceptional costs, separate from ordinary +1 updates.

Lifecycle comes from the importer's already-persisted native turn metadata. The compatibility decoder requires selected `codex-occurrences-v1` or `v2` evidence, the exact deterministic turn-note operation namespace, the full thread/turn scope, and the expected occurrence coordinate before interpreting the normalizer's status grammar. It never recognizes a task from arbitrary conversation text, silence, or elapsed time. Unsupported/missing status remains unknown. Failed/interrupted records are retained even if a task has no visible items. Durable operations and exporter output are unchanged by grouping.

Folding hides only safe straight interiors. Git nodes and attachment points, graph endpoints, forks/merges, every routing bend (including another lane passing through the row), failure/warning/cancellation outcomes, and explicitly unresolved tools/commands remain visible. The graph's real nodes, parents, and lane ownership are unchanged by disclosure. Native and WASM share this graph contract. Visible rank/select skips hidden members without walking a whole folded task during paging. Explicit toggles use the existing keyed DOM and motion path; search and raw/detail requests still target original item identities.

The implemented tests cover bounded membership edits at 10, 1,000 and 100,000 items; late interleaving and older backfills; rollback boundaries; authenticated legacy lifecycle notes and retraction; historical defaults, same-batch completion, late/revised members, search reveal, and late fork attachments. The real VS Code suite (`history-grouping.e2e.ts`) exercises imported completed history and subsequent source-file appends, verifies stable headers and lane centers, and records movement and growing strokes under Xvfb.

**Deferred:** automatic progressive folding based on exposure/reading state, hooks, Claude live collection, and any provider-neutral mapping of Claude execution boundaries. There is no timer or automatic folding transition in this first version. Claude's existing imported rows remain ungrouped in the retained live path until an explicit structural mapping is implemented.

## Earlier alternatives and evidence

The research below motivated separating membership from visibility. Its chat-based boundaries and numeric automatic-folding thresholds were exploratory and were superseded by the native-task decision above.

**Recommendation: show each new logical record immediately, mark related work without hiding it, then fold an older eligible portion behind one expandable summary.** Keep recent records exposed. Prefer a conversational or structural boundary as a settling signal, and protect what the viewer is inspecting. A timer can establish a minimum exposure period; elapsed time alone should not collapse work.

The main architectural distinction is **membership versus visibility**. The native service can know that records belong to the same work interval while the viewer still shows all of them. This preserves incremental collection and gives the presentation time to settle. There is no need to choose between one record per delta and useful grouping.

The strongest uncertainty is the automatic folding policy. The sources establish useful precedents and constraints, but none evaluates this exact agent-history interface. Recent-row counts, exposure delays, and compaction thresholds below are prototype parameters, not measured optima.

The investigation used the current working tree, existing grouping tests, live searches of developer-tool documentation, original visualization research, and W3C guidance. It covered explicit log sections, expanded versus collapsed defaults, reading a live stream, stable visual identity, and counter-evidence about animation. Product documentation is evidence of product behavior, not evidence that its design is best for EditChain. The graph-comprehension study was inspected through its institutional abstract; no claims about inaccessible experimental details are made. Hook installation and provider integration changes are outside this proposal.

**The old approach has a good boundary rule and an unsuitable live representation.** Its outer work groups cover connected, linear activity between user/agent conversation rows. Session creation, forks, merges, subagent/reconnect edges, and produced-commit endpoints prevent contraction. Even a singleton gets a work wrapper. Existing plan and execute bundles can become nested children. These behaviors are explicit in the [grouping pass](/mnt/hot/ambientlight/repos/editchain/crates/editchain-project/src/activity.rs:830) and [branch-boundary test](/mnt/hot/ambientlight/repos/editchain/crates/editchain-project/tests/activity.rs:680).

The old summary uses the newest member as its anchor, and synthetic graph keys derive from that anchor. Consequently, extending a group can change its identity. The new live path instead renders independent items and removes work-unit/session-summary annotations. Neither representation alone supplies a stable group that grows while retaining its members. [Group construction](/mnt/hot/ambientlight/repos/editchain/crates/editchain-project/src/activity.rs:1171), [node identity](/mnt/hot/ambientlight/repos/editchain/crates/editchain-project/src/node.rs:321), [live presentation](/mnt/hot/ambientlight/repos/editchain/crates/editchain-node/src/history/realtime/rows.rs:50).

**Several alternatives are viable, with different failure modes.** The judgments in this table are design analysis for EditChain.

| Approach | What happens to a new record? | What triggers folding? | Benefit | Main cost or failure mode |
| --- | --- | --- | --- | --- |
| Immediate collapsed group | Usually only the count changes | Membership on arrival | Compact at any rate | Violates the requirement that new records first appear individually |
| Fixed delay or quiet period | Appears, then disappears into a summary | Record age or no arrivals for a duration | Easy to explain and implement | Can hide unread output; a slow command looks like an interval ending; repeated pauses cause repeated contraction |
| Collapse at chat/turn boundary | Remains exposed throughout the interval | A subsequent chat or explicit completion | Clear semantic rhythm, little churn while work runs | Long turns remain large; the last record can disappear immediately if completion arrives in the same batch |
| Keep the latest K records | Appears in a visible recent portion | Each later arrival retires an older row | Compact during long turns | Exposure time depends on arrival rate; a burst can retire a record before a paint |
| Fold only outside the reading area | Appears and remains until eligible and outside the viewport | Viewer moves on, or arrivals move older content away | Predictable reading; preserves user control | History stays more expanded while it is being inspected |
| Manual folding only | Always remains individually accessible | Explicit user action | Strongest control; useful comparison baseline | Repetitive cleanup and substantial visual noise |
| Separate live feed and grouped graph | Appears in a separate feed | Graph can group immediately | Can optimize reading and graph density independently | Two representations to reconcile; duplicated selection and attention |

My preferred combination is a recent exposed portion, semantic boundaries, and protection for the reading area. A boundary makes an interval eligible to settle; it does not prove that the viewer has read its contents. Count and time thresholds are additional eligibility gates, not permission to hide an unseen record.

**Existing tools support separating the container from its initial visibility.** Chrome distinguishes `console.group()` from `console.groupCollapsed()`. GitLab independently distinguishes section start/end markers from its collapsed-default flag, and GitHub supports explicit expandable log intervals. These are three product precedents for known membership with controllable disclosure. They do not prescribe an automatic folding delay. [Chrome Console API](https://developer.chrome.com/docs/devtools/console/api#consolegrouplabel), [GitLab job logs](https://docs.gitlab.com/ci/jobs/job_logs/#create-custom-collapsible-sections), [GitHub workflow commands](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-commands#grouping-log-lines).

VS Code's terminal uses a command guide and sticky command context to show ownership of output without collapsing it. This supports an initial lightweight bracket or work label around visible records. Grafana's live log view highlights arrivals and allows pausing or scrolling to inspect earlier content. My inference is that EditChain should distinguish collection from following the live edge: reading history should not stop ingestion or pull the viewport back to new arrivals. [Terminal shell integration](https://code.visualstudio.com/docs/terminal/shell-integration), [Grafana live tailing](https://grafana.com/docs/grafana/latest/visualizations/explore/logs-integration/#live-tailing).

**A proposed progression uses four visible states.** Membership may already exist in the first two; no separate group node is necessary until there is something to fold.

| State | Visible behavior | Transition condition |
| --- | --- | --- |
| New | Full row and growing connection; stable item identity | Record is admitted to the live presentation |
| Open work | Related rows share a subtle bracket/label; all remain exposed | A causal work interval is recognized |
| Partly folded work | One summary for older members plus individually visible recent members | An eligible older prefix can fold safely |
| Settled work | One expandable summary; important exceptions remain visible | Interval is closed and its remaining members are eligible; explicit expansion overrides this |

Read this example in chronological order; the extension can retain its existing newest-first order:

```text
User request
  Read importer
  Inspect cursor
  Search call sites
  Run checks
  Inspect result

Later, after the older portion has been exposed and left the reading area:

User request
  [3 earlier activities ▸]
  Run checks
  Inspect result

After an agent response and the viewer has moved on:

User request
  [5 activities · checks passed ▸]
Agent response
```

The summary represents only the folded portion while the group is partly exposed, so counts do not appear to duplicate the visible recent rows. The whole work interval can retain an overall count in its label. A singleton should stay an ordinary row; a wrapper must save space or supply useful navigation. Start with one work disclosure level. Preserve each item's own details and exact provider identity, but avoid automatically adding work → execute bundle → plan bundle navigation layers. An optional repetition view can be considered later.

**The default automatic rule should be conservative.** As a starting experiment, protect the latest three semantic records per active interval and require at least two eligible records before making a folded prefix. Also require a minimum foreground exposure period, initially two seconds after arrival motion ends. These numbers are adjustable experiment inputs. Protect every row currently intersecting the viewport, the scroll anchor, keyboard focus, text selection, search reveal, and anything the user explicitly expanded. A row is not eligible while its represented operation is known to be running. Where lifecycle state is unavailable, use a later boundary rather than inventing completion from quiet time.

A completed chat item, next user message, or structural endpoint can close an interval. Streaming revisions of that same chat must not repeatedly close and recreate it. Elapsed time never establishes semantic completion. On a new arrival, update the model immediately; schedule eligible compaction separately after the arrival motion has finished. During a continuous stream, coalesce compactions at safe opportunities rather than restarting a contraction for every record.

Under this conservative default, small intervals may remain fully expanded while on screen. That is intentional. The bracket provides grouping before any rows disappear, and manual folding remains available. An optional more aggressive mode could fold visible older rows after a dwell, but it should be evaluated against this default, not assumed to be an improvement.

**“New records always show” needs an explicit contract.** Here a record means a newly admitted logical history item, rather than every raw JSONL envelope, duplicate observation, or text revision. Existing source normalization remains responsible for those distinctions.

- In following mode, a new item must enter an exposed row, even if its owning group was already collapsed. Reveal the new member without reopening every old member.
- A changed existing item retains its row identity. A material late result inside a folded group gets an exposed update and an updated marker; text chunks do not become separate work rows.
- A received delta or `liveSettled` acknowledgement is not proof of exposure. Track which item revisions have actually been painted in the foreground viewport. Exposure is not proof of comprehension; it only prevents collapsing something that never had an opportunity to be seen.
- While the viewer reads older history, keep its position and show a pending-arrival count with a way to return to live. Pending items remain individually revealable and do not become eligible merely because a timer ran in the background.
- Initial historical loading uses the normal settled grouping policy. Returning after the panel was hidden must distinguish that baseline from records arriving since the viewer's last acknowledged position.
- At overload, preserve all items and expose the backlog. A finite viewport cannot make every record simultaneously readable at an unbounded arrival rate. Do not silently replace a burst with a count or delay durable ingestion to play it one row at a time.

For example, keeping the latest three records at a steady 20 records/second gives each only about 150 ms before it falls out of that set. Giving every record a two-second eligibility delay instead requires retaining approximately 40 recent records, before any other protection. These are arithmetic examples, not measured Codex rates. The recent-row number must therefore be a minimum protection, not a hard visibility cap.

**Keep causal grouping exact and visual exceptions explicit.** Group only an adjacent linear causal run in the displayed chronology, within the same source/session and compatible turn context. Never group solely by elapsed time, matching text, tool name, or turn ID. A turn can contain multiple conversations and branches. Concurrent sessions remain in chronological order; do not gather their members into a contiguous block by moving unrelated rows. Start by retaining separate contiguous fragments when streams interleave.

Keep conversation rows, session boundaries, Git commits and their attachment points, and fork/merge endpoints outside foldable interiors. For the first version, keep failures, required user decisions, and unresolved operations exposed as additional breaks. Ordinary file changes and successful verification may fold after exposure, with explicit file/change and outcome counts and direct detail access in the summary. Do not imply that a failed check succeeded just because a later unrelated check passed. These are proposed refinements to the old broad work contraction.

**Incremental grouping should reference independently retained items.** The current `LiveBlock` includes all rows in the block, and the renderer validates/prepares the entire incoming block. Simply turning a growing work group into one `LiveBlock` would make ordinary append cost proportional to group size. That would defeat the intended +1 behavior. [Live protocol](/mnt/hot/ambientlight/repos/editchain/crates/editchain-protocol/src/live.rs:61), [live expansion preparation](/mnt/hot/ambientlight/repos/editchain/crates/editchain-history-renderer/src/app/expansion/live.rs:187).

The proposed model has three independent parts:

| Part | Owner | Incremental responsibility |
| --- | --- | --- |
| Item content and canonical relationships | Existing native projection | Upsert/retract only changed items and affected ancestry |
| Group membership and summary aggregates | Native grouping index | Add/remove a member, extend a range, update counts, close or split a group |
| Exposure and disclosure | Each viewer | Protect recent/inspected revisions, fold an eligible interval, preserve explicit user choices |

Give groups their own stable key derived from a durable opening boundary and initial member identity, scoped by source generation and grouping-rule version. Do not use the newest member or an absolute display row. Appending must retain that key. Late boundaries and topology corrections need explicit split/merge continuity rules; ordinary appends must not silently reconstruct every group. A canonical bootstrap computes the same grouping rules; local exposure and expansion may legitimately differ between viewers.

Extend the revisioned protocol with group metadata/membership edits alongside item edits. For an ordinary append, aim to publish the new/changed item, its membership insertion, and the affected summary counters. Do not resend an ever-growing array of all members. Represent ordered membership with retained ranges or a tree that supports split/concatenate and aggregate counts. Cross-item disclosure requires extending the current block-local expansion index. It is not a CSS-only repair.

The target is logarithmic index work plus the actually affected records and graph routes. This is an implementation objective, not a proven bound for every edit: a late fork, retraction, new chat boundary, or interleaving insertion can require splitting an existing group. Measure those cases separately. Semantic group edits remain in epoch/revision replay; local exposure timers must not generate canonical operations or change the global history revision. Search and raw/detail access continue to address original item keys, including folded members.

**The graph needs a view contraction, with the underlying topology preserved.** A folded run substitutes a summary between the same external endpoints. Keep the constituent item nodes and exact edges in the retained model so expansion can restore them. Preserve lane ownership and the existing fixed lane pitch; folding changes vertical extent. Late discovery of a branch inside a folded interval must split/reveal the affected interval before displaying the new connection. Never draw a branch from a generic summary if that hides its actual branching point.

Keep the flat keyed DOM for rows, even if the semantic model has groups. Appending a member or creating a bracket should not detach/reparent all its siblings. Draw a new connection using the existing arrival animation. If an explicit fold is visible, contract the eligible vertical interval toward its summary with endpoints held on their lanes; keep the reading anchor stationary. Automatic folding outside the viewport needs no decorative animation. A new arrival during a fold must remain exposed and must not restart unrelated SVG animation clocks.

Stable keys are a practical technique for preserving element identity across transitions; D3's own join documentation and its author's explanation provide a direct precedent. This is a design principle to apply to the existing Rust/WASM renderer, not a proposal to introduce D3. [D3 data joins](https://d3js.org/d3-selection/joining), [Bostock, Object Constancy](https://bost.ocks.org/mike/constancy/).

**Animation is useful feedback, but cannot carry the only evidence of change.** Heer and Robertson's 2007 experiments found benefits for suitably designed animated chart transitions, but complex staging could increase error. Their study does not establish a folding delay for readable text. The independent 2011 dynamic-graph study found faster responses with small multiples, while animation reduced errors for certain change-detection tasks; preserving the mental map had little measured effect in those conditions. This argues for testing actual EditChain tasks rather than assuming smoother motion proves better comprehension. [Heer and Robertson, paper and discussion](https://idl.cs.washington.edu/files/2007-AnimatedTransitions-InfoVis.pdf), [Archambault, Purchase and Pinaud, institutional abstract](https://research.monash.edu/en/publications/animation-small-multiples-and-the-effect-of-mental-map-preservati/).

Phosphor explored leaving an explanation of a change after showing its result immediately. I infer that a persistent new/updated count and retrievable member list can complement EditChain's brief motion. This does not require copying Phosphor's visual treatment. W3C's guidance also supports user control over automatic updates; reduced motion and an explicit presentation pause should be usable while collection continues. [Phosphor research](https://www.microsoft.com/en-us/research/publication/phosphor-explaining-transitions-in-the-user-interface-using-afterglow-effects/), [W3C Pause, Stop, Hide](https://www.w3.org/WAI/WCAG22/Understanding/pause-stop-hide.html).

The selective evidence map below distinguishes established premises from the proposal.

| Claim | Direct evidence | Independent check or limitation | Assessment |
| --- | --- | --- | --- |
| The live path bypasses the old grouping | Local live presenter and grouping pass linked above | Existing grouping and branch tests express earlier behavior | High confidence, inspected source |
| Membership can exist while records remain expanded | Chrome group API | GitLab section controls and GitHub expandable intervals | High confidence as product behavior |
| Live monitoring benefits from a deliberate reading state | Grafana pause/scroll behavior | W3C addresses automatic changes and user control | Supported precedent; proposed EditChain behavior still needs evaluation |
| Stable item keys support retained DOM identity | D3 join API and Bostock explanation | Same source family; local renderer already uses continuity keys | High confidence for mechanism, not a usability guarantee |
| More elaborate animation is not automatically better | Heer/Robertson discussion | Independent Archambault/Purchase/Pinaud graph tasks show tradeoffs | Supported with task-specific limits |
| One whole-group upsert would grow with group size | `LiveBlock.rows` and full-block preparation | Directly follows from this representation | High confidence; replacement protocol performance unmeasured |
| Protected recent rows plus deferred folding is the best default | Synthesis of the above and the user's visibility requirement | No direct comparative study of EditChain | Recommendation with medium confidence; timing remains open |

**I would implement this gradually in three reviewable steps.** First restore work membership, labels and explicit folding with stable groups and member deltas. Leave live arrivals exposed. This checks semantic grouping and graph contraction before automatic timing is involved. Next add boundary-triggered eligibility, protected recent rows, and automatic folding outside the reading area. Finally compare the conservative default with boundary-only and more aggressive recent-row folding in the actual VS Code harness. Keep the manual mode as a control. Hooks can be added independently.

The next evidence should be recorded task performance and invariants, using slow records, an interrupted command, a sustained long turn, a dense burst, interleaved sessions, a late result, and a fork discovered inside old work. Include these acceptance checks:

- No fresh item is automatically folded before its required foreground exposure; no exposure is credited while the panel is hidden.
- Same-paint arrivals plus a turn boundary still expose those arrivals. Duplicate replay creates no second arrival or renewed highlight.
- An append preserves existing item/group keys, selection, expansion, and unaffected DOM/SVG nodes. Frozen summaries receive late-member updates without reopening everything.
- Collapsing/expanding preserves causal attachment points, lane pitch, and the reader's anchor. Explicitly inspected content never auto-collapses.
- A +1 append to a group of 10, 1,000, and 100,000 members does not resend or reprocess the whole member list. Report work counters and payload bytes, including exceptional split cases.
- Search reveals the exact folded item; one-level disclosure still exposes all original records and details. Reduced motion changes presentation only.
- In a short comparison, measure whether the viewer can identify the latest action, a failed check, changed files, and the branch that produced a commit; measure time and errors, not animation preference alone.

If conservative folding leaves too much noise, test the aggressive policy. If readers repeatedly reopen automatically folded groups or miss errors, retain more exposed work or move the trigger toward explicit action. The most important unresolved choice is how much visible older work the user wants to allow the extension to fold automatically; the conservative default leaves that choice open.

The principal external sources are linked at their claims above: current Chrome, GitLab, GitHub, VS Code, Grafana and W3C documentation; D3's join reference and Bostock's 2012 article; Heer and Robertson (2007); Archambault, Purchase and Pinaud (2011); and Baudisch and colleagues' Phosphor work (2006). Multiple pages about the same study or product are not treated as independent corroboration.
