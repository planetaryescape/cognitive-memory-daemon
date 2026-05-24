---
name: cognitive-memory
description: "Use the cognitive-memory daemon CLI (cm) to store and search durable memories shared across AI agents on this machine. Invoke when the user asks to remember a fact, save a preference, recall something they told you, store a note for later, find something in memory, look up what they know about X, search across past memories, or check daemon status. Triggers: 'remember this', 'save to memory', 'store this', 'note this', 'remind me later', 'recall', 'what do I know about', 'search memory', 'find memory', 'cognitive memory', 'agent memory', 'ai memory', 'shared memory', 'cm', 'cm store', 'cm search', 'cm status'."
---

# cognitive-memory CLI (`cm`)

Local, always-on memory service for AI agents. The daemon (`cm-daemon`) owns
a SQLite store + an embedding model and serves multiple AI clients on the
same machine. The `cm` CLI is the canonical scripting and automation
surface — `cm-http` exists for browser clients but every capability lands
in `cm` first.

## Quick reference

```bash
# Read
cm status                              # Daemon health + memory count
cm counts                              # Per-tier counts (hot/cold/stub/total)
cm search "favourite drink"            # Semantic search
cm search --hybrid "rust async tokio"  # Dense + BM25 fused
cm search-lexical "specific phrase"    # BM25-only
cm get <id>                            # Fetch one memory
cm get-many <id1> <id2> ...            # Fetch many
cm list --type preference --limit 20   # Filter list
cm linked <id>                         # Memories linked from id
cm find-fading --max-retention 0.3     # Consolidation candidates
cm find-stable --min-stability 0.85 --min-access-count 10  # Core promotion candidates

# Write — single
cm store "User prefers tea over coffee."
cm store --type preference "Standup at 09:00 daily."
cm store --core "User's full name is Bhekani Khumalo."     # Synaptic tagging (paper §3.4)
cm update <id> --content "New text" --importance 0.9
cm delete <id>

# Write — batch (paper §3.6: co-creation auto-associates pairs)
cm store-batch "Fact A" "Fact B" "Fact C" --link-weight 0.5
# Returns 3 ids + 6 associations (3 pairs × bidirectional)

# Links
cm link <id1> <id2> --strength 0.5
cm unlink <id1> <id2>

# Lifecycle
cm cold <id>                           # Migrate to cold storage
cm hot <id>                            # Restore from cold
cm stub <id> "Brief summary"           # Convert to archival stub
cm mark-superseded <summary_id> <id1> <id2>
cm retention <id> 0.6                  # Set retention floor
cm clear --confirm                     # DELETE EVERYTHING under user_id

# Bridge token (for cm-http)
cm mint-token --scope write --ttl-seconds 86400

# Multi-tenant
cm --user-id work search "code review backlog"
cm --user-id personal store "Birthday: 1990-04-12."
```

## Important patterns

1. **`cm store-batch` is the way to capture related facts together.**
   When the user mentions multiple things in the same breath, use
   `store-batch` instead of N separate `store` calls. The daemon creates
   bidirectional associations between every pair (paper §3.6 — "memories
   form bidirectional associations when they are retrieved together OR
   created in the same context"). Later, `cm linked <id>` will surface
   the co-created peers.

2. **`cm store --core` is synaptic tagging.** Use it for identity-critical
   information: the user's name, allergies, fundamental preferences, family
   relationships. The daemon sets `retention_floor = 0.6` at encoding so
   the memory can never decay below 60% (paper §3.4). Use sparingly —
   most memories should earn core status through repeated retrieval, not
   be assigned at storage.

3. **Memory IDs are ULIDs.** `mem_01KR0FVJ3ZHG0NTZ44FD49DBDQ`. Get them
   from the `stored:` line of `cm store` or from `cm search --json` results.

2. **`--json` for any read command** — `cm status --json`, `cm search --json
   ...`. Use this when piping to `jq` or back into another agent.

3. **Daemon auto-starts.** The first `cm` command that needs the daemon
   spawns `cm-daemon` in the background and waits for the socket. Use
   `--no-spawn` when you want the command to fail fast if no daemon is
   running (CI, scripts that should not introduce side effects).

4. **`--hybrid` for keyword-heavy queries.** Default search is pure dense
   (semantic). Add `--hybrid` to fuse BM25 (literal token match) — useful
   when the query has rare technical terms, proper nouns, or exact phrases
   the embedding model dilutes.

5. **`--user-id`** is the hard tenancy boundary. Memories under one
   `user_id` are not visible from another. `default` is the default. Use
   per-context user-ids (`work`, `personal`, project name) when you want
   isolation.

6. **Categories and types** are the v6 memory model:
   - `--category`: `episodic` | `semantic` | `procedural` | `core`
   - `--type` (`memory_type`): `fact` | `preference` | `plan` | `transient_state` | `other`

   Defaults: `category=semantic`, `type=fact`. Override when you have a
   reason — e.g. `--type preference` for user preferences.

## Typical workflows

### Remember something the user said
```bash
cm store --type preference "User prefers tea over coffee."
# stored: mem_01KR0FVJ3ZHG0NTZ44FD49DBDQ
```

### Recall before answering
```bash
cm search --json --limit 5 "user beverage preference"
# Returns top-5 ranked memories under default user_id
```

### Project-scoped notes
```bash
cm --user-id default store \
  --metadata '{"project":"cognitive-memory","source":"plan-review"}' \
  "Decided to vendor mxr's two-pool wrapper rather than reimplement."

cm search "vendor decisions"
```

### Confirm daemon is healthy before a long batch
```bash
cm --no-spawn status || { echo "daemon not running"; exit 1; }
for fact in "${facts[@]}"; do
  cm store "$fact"
done
```

### Pipe search results into another agent
```bash
cm search --json "deployment runbook" \
  | jq -r '.results[].content' \
  | head -3
```

## When to use cm vs not

**Use `cm`** when:
- The user says something worth remembering across sessions ("I prefer X", "deadline for Y is Z")
- You need to recall what the user previously told you about a topic
- Multiple agents on this machine should see the same memory
- The fact is durable (not just relevant to the current conversation turn)

**Skip `cm`** when:
- The information is ephemeral (current task state — that's working memory, not long-term)
- The user is asking for general world knowledge, not their own context
- The fact is already obvious from the current conversation and won't be useful later

## Full command reference

See [references/commands.md](references/commands.md) for every flag and
option, including the HTTP bridge (`cm-http`) and daemon binary (`cm-daemon`).
