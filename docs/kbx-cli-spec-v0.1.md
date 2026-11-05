# kbx CLI spec v0.1 (as planned)

This is the design spec the TeamMesh tasks cite as "§n". It is kept verbatim
below; the project's fixed decisions override it where they differ:

- No MCP server (§9, M5) in this project; `crates/kbx-mcp` is not created.
- The binary crate is `crates/kbx` (not `kbx-cli`); the binary and crates.io
  name is `kbx`.
- npm: unscoped shim `kbx-cli`, which installs the `kbx` command, plus platform
  packages `kbx-darwin-arm64`, `kbx-darwin-x64`, `kbx-linux-x64-gnu`,
  `kbx-linux-arm64-gnu`, `kbx-windows-x64` (not `@kbx/cli-*`); hand-written
  GitHub Actions, no cargo-dist, no Homebrew, no postinstall. npm refuses the
  name `kbx` as too similar to existing packages (see docs/releasing.md).
- Search: the built-in engine is the default, and `kbx init` writes
  `search = "builtin"`. QMD (`@tobilu/qmd`) was the v0 backend and stays a
  supported option for good: with `search = "qmd"` it's installed silently when
  missing. See `docs/search-engine.md`.
- Correction: §5 used to say `qmd query --filter`. No qmd release has that
  flag, and qmd ignores flags it doesn't know, so it would limit nothing.
  Searches use `-c <collection>`.
- Each vault has its own qmd index, `--index kbx-<hash of the vault path>`, so
  `qmd update` and `qmd embed` never reach the user's other collections.
  `kbx init` creates a collection per scope and `qmd.roots` in `kbx.toml` maps
  each one to its folder (§10).
- `judge.min_confidence` is dropped from the example ontology: confidence is
  stored but never used for auto-accept (§6.3).
- Licence: AGPL-3.0, no CLA.
- §14's section-level question is settled in `docs/sections.md`: anchors are
  opt-in (`write.section_anchors` in `kbx.toml`), `{id}#^{block-id}` is the
  portable form, and a heading-slug path is the fallback address, written to
  frontmatter as a heading link.
- §14's shared embedding cache question is settled in `docs/embedding-cache.md`:
  the measured cost is `qmd query`, not embedding, so kbx caches search results
  in `.kbx/search-cache.sqlite`. QMD keeps the vectors, and kbx doesn't read or
  write its index. §5's process budget gains one or two `qmd status` calls.
- §3's `id`, `type` and `title` are optional. What a note doesn't state is
  inferred, never written: `id` from the path, `title` from the first H1 or the
  filename, `type` from `default_type` in `kbx.toml`. A file with no frontmatter
  is a note. See `docs/existing-vaults.md`.
- `search = "builtin"` selects kbx's own engine, in `.kbx/search-index.sqlite`, for
  the same `qmd_knn` generator. With no `search` key, a `kbx.toml` listing qmd
  collections stays on QMD and any other gets the built-in engine. See
  `docs/search-engine.md`.
- §2's "reject nested maps" holds only for `id`, `type`, `title`, `aliases` and
  `tags`. A vault's own keys are ignored whatever they hold, and a relation key
  holding a map is a `rel.unknown` finding. See `docs/frontmatter-writer.md`.
- `kbx.toml` lives in `.kbx/`: `kbx init` creates nothing else at the vault root
  and `--in-kbx` is a no-op kept for scripts. A root `kbx.toml` is still read, with
  a `kbx doctor` warning, and `kbx doctor --fix` moves it. Wherever this spec says
  `kbx.toml`, read `.kbx/kbx.toml`.
- §3, §6 and §7's writes to notes are opt-in. By default everything kbx would add
  to frontmatter stays in `.kbx/` and no `.md` file changes: accepted relations and
  their inverses, the review lock, and the inferred `id`, `type` and `title`.
  `kbx accept` and `a` in `kbx review` add an edge of origin `kbx`, which `kbx index`
  keeps and generators, `kbx lint` and `kbx export` take as they do a frontmatter
  relation. A note that states the relation wins, and the `.kbx/` copy is dropped.
  `[write] frontmatter = true` turns on the writer as §7 describes it, `--stage` and
  `kbx sync-stage` included, which refuse without it. `kbx apply` writes what's
  already kept in `.kbx/`, and `kbx mark-reviewed` sets the lock in either mode.
  See `docs/review.md` and `docs/frontmatter-writer.md`.
- §5's `common_neighbors`, `adamic_adar` and `two_hop_bridge` also read inline
  links, at `generators.mention_weight` in `kbx.toml` (0.25, and 0 for typed
  edges alone). A typed pair scores as it did. A note with too many inline links
  is a list, and its inline links are ignored rather than damped. See
  `docs/generators.md`.

---

# kbx — Rust CLI for an ontology-linked markdown knowledge base

> Working name `kbx` (knowledge-base exchange). Rename freely; every occurrence is in one constant.
> Status: design spec, v0.1. Everything below is a proposal to be cut down, not a contract.

## 1. What it is

A single static binary, distributed through npm, that turns a folder of markdown notes into a typed knowledge graph and keeps it healthy:

1. **Compile** frontmatter into a SQLite graph (`nodes`, `sections`, `edges`) validated against a small ontology.
2. **Suggest** missing links from several deterministic generators (graph structure, schema gaps, alias co-occurrence, embedding kNN via QMD), each with provenance.
3. **Judge** every candidate with an LLM — Claude Code CLI or Codex CLI when the user is logged in, a local model otherwise — producing accept/reject plus a relation type from the fixed vocabulary.
4. **Review** proposals in a queue; write accepted relations (and their inverses) to frontmatter; remember rejections.
5. **Serve** the graph to agents over MCP (stdio), alongside QMD's search.

Files stay the only source of truth. Every index is a disposable cache, rebuilt from the markdown.

### Non-goals (v0.x)

- Not a search engine. QMD owns full-text + vector search; `kbx` shells out to it.
- Not a GUI. Review happens in the terminal or through any editor via a staging frontmatter key.
- Not a sync layer. TeamMesh (or git) moves files; `kbx` indexes whatever is on local disk (Option A: per-user index, ACL enforced by what exists on disk).
- Not an entity extractor. Relations come from authors and from judged candidates between *existing* notes, never from LLM free-text extraction.

## 2. Repository layout

```
kbx/
├── Cargo.toml                 # workspace
├── crates/
│   ├── kbx-core/              # parsing, ontology, SQLite schema, graph ops (no I/O to models)
│   ├── kbx-gen/               # candidate generators
│   ├── kbx-judge/             # judge trait + backends: claude-cli, codex-cli, ollama, llama-server
│   ├── kbx-mcp/               # MCP stdio server (rmcp)
│   └── kbx-cli/               # clap binary; the only crate that prints
├── npm/
│   ├── kbx-cli/               # JS shim package: bin/kbx.js resolves the platform binary
│   ├── kbx-darwin-arm64/      # optionalDependency packages, one per target, binary only
│   ├── kbx-darwin-x64/
│   ├── kbx-linux-x64-gnu/
│   ├── kbx-linux-arm64-gnu/
│   └── kbx-windows-x64/
├── schema/
│   ├── ontology.example.yaml
│   └── judge.schema.json      # JSON Schema the judge must return
├── prompts/
│   └── judge.system.md        # stable prefix (ontology + rules + examples) → prompt-cache friendly
└── docs/
```

**Key crates**

| Purpose | Crate | Notes |
|---|---|---|
| CLI | `clap` (derive) | subcommands below |
| Frontmatter | `serde_yaml` + hand-rolled `---` splitter | reject nested maps per ontology rules |
| Markdown headings/blocks | `pulldown-cmark` | heading path + `^block-id` capture, byte offsets |
| SQLite | `rusqlite` (bundled) | one file `.kbx/graph.sqlite`; WAL mode |
| Graph algorithms | `petgraph` + own Adamic–Adar/CN/bridge code | graph fits in RAM at 100k nodes |
| File watching | `notify` + `notify-debouncer-full` | fallback: periodic hash reconciliation |
| HTTP (Ollama / llama-server) | `reqwest` (rustls) | no OpenSSL dependency |
| Subprocess (claude/codex/qmd) | `tokio::process` | stdin for prompt, JSON on stdout |
| MCP | `rmcp` (official Rust SDK) | stdio transport only in v0 |
| Hashing / IDs | `sha2`, `uuid` (v5) | deterministic section IDs |
| Logging | `tracing` + `tracing-subscriber` | `--log json` for agents |

## 3. Authoring conventions the tool enforces

```yaml
---
id: 01J9Z8K3M2Q4R5S6T7V8W9X0Y1     # ULID; also the filename prefix. Never changes.
type: Procedure                    # must be a class in ontology.yaml
title: OAuth 2.0 authorization flow
aliases: [oauth flow, authorization code grant]
tags: [auth, security]
depends_on: ["[[01J9Z8…tokens]]", "[[01J9Z8…redirects]]"]   # relation keys = lists of quoted wikilinks
part_of: ["[[01J9Z8…overview]]"]
kbx:
  proposed:                        # staging area written by `kbx suggest`; never read by the compiler
    - {rel: see_also, to: "[[01J9Z8…pkce]]", by: [adamic_adar, judge], score: 0.81, id: 7f3a…}
  reviewed: 2026-11-03             # optional lock: skip re-suggestion until content hash changes
qmd:
  metadata: {type: Procedure, tags: [auth, security]}   # mirrored for QMD metadata
---
```

- Links resolve by `id` first, then by filename/alias. Filename-only links are flagged by `kbx lint` as `link.unstable`.
- Relations live in frontmatter keys (the only form Obsidian Bases, Breadcrumbs, Dataview and this tool all read). Inline `[[links]]` in the body are indexed as untyped `mentions` edges.
- Sections are addressable as `{id}#{heading-slug-path}` and, when present, `{id}#^{block-id}`. The compiler also assigns `UUID5(id + heading path + ordinal)` as the stable section key.
- The `kbx:` and `qmd:` namespaces are tool-owned; `kbx` is the only writer of `kbx:` and mirrors the filterable subset of ontology fields into `qmd.metadata` on write.

### `ontology.yaml`

```yaml
version: 1
classes:
  Concept:   {description: "A definable idea or term."}
  Procedure: {description: "Steps to achieve an outcome.", requires: [depends_on]}
  Decision:  {description: "A choice made, with rationale."}
  Entity:    {description: "A system, team, person, or product."}
  Reference: {description: "External source material."}
relations:
  part_of:     {inverse: has_part,     domain: [any],       range: [any],       weight: 1.0, transitive: true}
  depends_on:  {inverse: required_by,  domain: [Procedure], range: [Concept, Procedure, Entity], weight: 0.9}
  implements:  {inverse: implemented_by, domain: [Procedure, Entity], range: [Concept], weight: 0.8}
  supersedes:  {inverse: superseded_by, domain: [Decision], range: [Decision], weight: 0.8, exclusive: true}
  contradicts: {inverse: contradicts,  domain: [any],       range: [any],       weight: 0.7, symmetric: true}
  example_of:  {inverse: has_example,  domain: [any],       range: [Concept],   weight: 0.6}
  see_also:    {inverse: see_also,     domain: [any],       range: [any],       weight: 0.3, symmetric: true}
judge:
  vocabulary: [part_of, depends_on, implements, supersedes, contradicts, example_of, see_also, none]
  min_confidence: 0.6
  direction: from_schema          # direction is decided by domain/range, never by the model
```

Validation rules derived from this file: unknown class → `type.unknown`; relation key not in vocabulary → `rel.unknown`; target class outside `range` → `rel.range`; `requires` unmet → `schema.gap` (which is also a candidate generator, see §5); `exclusive` relation with >1 target → `rel.cardinality`.

## 4. SQLite schema (`.kbx/graph.sqlite`)

```sql
CREATE TABLE files    (path TEXT PRIMARY KEY, id TEXT UNIQUE, type TEXT, title TEXT,
                       content_hash TEXT, fm_hash TEXT, mtime INTEGER, scope TEXT, indexed_at INTEGER);
CREATE TABLE aliases  (alias TEXT, id TEXT, PRIMARY KEY(alias, id));
CREATE TABLE sections (key TEXT PRIMARY KEY, id TEXT, heading_path TEXT, block_id TEXT,
                       start_line INTEGER, end_line INTEGER, text_hash TEXT, tokens INTEGER);
CREATE TABLE edges    (src TEXT, dst TEXT, rel TEXT, inverse_of TEXT, weight REAL,
                       origin TEXT CHECK(origin IN ('frontmatter','inline','derived')),
                       src_section TEXT, line INTEGER, PRIMARY KEY(src, dst, rel, origin));
CREATE INDEX edges_dst ON edges(dst);                 -- forgetting this is the classic mistake
CREATE TABLE candidates (cid TEXT PRIMARY KEY, src TEXT, dst TEXT, generators TEXT /*json*/,
                       features TEXT /*json*/, score REAL, created_at INTEGER);
CREATE TABLE verdicts (cid TEXT, backend TEXT, model TEXT, prompt_hash TEXT, verdict TEXT,
                       rel TEXT, confidence REAL, rationale TEXT, cost_usd REAL, cached_tokens INTEGER,
                       created_at INTEGER, PRIMARY KEY(cid, backend, model, prompt_hash));
CREATE TABLE reviews  (src TEXT, dst TEXT, rel TEXT, decision TEXT CHECK(decision IN ('accept','reject','defer')),
                       reason TEXT, strikes INTEGER DEFAULT 0, decided_at INTEGER, PRIMARY KEY(src, dst, rel));
CREATE TABLE meta     (k TEXT PRIMARY KEY, v TEXT);  -- ontology_hash, pipeline_version, judge prompt hash
```

The `reviews` table is the project's most valuable artifact: it is the evaluation set for every generator and the training set for a future small classifier. `kbx export reviews --jsonl` dumps it.

## 5. Candidate generators (`kbx-gen`)

Each generator implements:

```rust
pub trait Generator {
    fn name(&self) -> &'static str;
    fn run(&self, g: &Graph, scope: &Scope) -> Vec<Candidate>;   // Candidate { src, dst, score, features }
}
```

Candidates from all generators are unioned on `(src, dst)`; `generators` records every contributor and `features` their raw scores. Pairs present in `reviews` as `reject` (or already linked) are dropped before judging. Default budget: ≤ 40 candidates per note, ≤ 2,000 per run.

| Generator | Signal | Why it finds what embeddings miss |
|---|---|---|
| `common_neighbors` / `adamic_adar` | shared typed neighbours, hubs downweighted | structure, not vocabulary |
| `two_hop_bridge` | A→X←B where A,B unlinked; weighted by relation weights | the "both depend on the same concept" case |
| `schema_gap` | node violates `requires` (e.g. Procedure without `depends_on`); candidates = nodes whose class is in the relation's `range`, same scope, ranked by other signals | uses the ontology as a hypothesis generator |
| `alias_cooccur` | FTS hit of another note's `title`/`aliases` in this note's body with no link | exact, free, style-independent |
| `qmd_knn` | `qmd --index <vault index> query --json -n 20 -c <collection>` per note, least linked first, at most 100 per run; HyDE sub-queries optional. With `search = "builtin"`, the same loop over kbx's own index (`docs/search-engine.md`) | topical neighbours; the hit side is section-level |
| `type_pair_prior` (optional) | learned from `reviews`: P(rel \| type(src), type(dst)) | cheap, improves over time |

Hub damping: any node with degree > p99 is downweighted in `adamic_adar` and excluded as a `two_hop_bridge` intermediary unless the relation is `part_of`.

## 6. Judge (`kbx-judge`)

```rust
#[async_trait]
pub trait Judge {
    fn id(&self) -> JudgeId;                       // "claude-cli", "codex-cli", "ollama", "llama-server"
    async fn available(&self) -> Availability;     // Ready | NotLoggedIn | NotInstalled | Unreachable
    async fn judge(&self, batch: &[CandidateView], ctx: &JudgeContext) -> Result<Vec<Verdict>>;
}
```

### 6.1 Backend selection

Order, unless overridden by `--judge` or `kbx.toml`:

1. `claude-cli` — `claude` on PATH **and** logged in
2. `codex-cli` — `codex` on PATH **and** logged in
3. `ollama` — `OLLAMA_HOST` or `http://127.0.0.1:11434` answers `/api/tags` and has the configured model
4. `llama-server` — `http://127.0.0.1:8080/v1/models` answers
5. none → `kbx suggest` still runs generators and writes unjudged candidates with `by: [generators]`; review is manual

**Login detection** is two-stage and cached in `.kbx/judge-cache.json` for 24 h:

- Fast check: `claude` → presence of `~/.claude/.credentials.json` or a keychain entry (macOS) and `claude --version` succeeding; `codex` → `~/.codex/auth.json` present. These only rule *out*.
- Probe: one minimal call — `claude -p "reply with {\"ok\":true}" --output-format json --json-schema '{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}' --max-turns 1` and `codex exec --json --output-schema probe.json --ephemeral --skip-git-repo-check "reply with ok"` — treating `authentication_failed` / non-zero exit as `NotLoggedIn`. A probe costs a few hundred tokens and runs once a day.
- `kbx doctor` prints the result for all backends and which one would be chosen.

Per-scope override: `kbx.toml` can pin `judge = "ollama"` for folders marked sensitive, so those notes never leave the machine even when a cloud CLI is available.

### 6.2 Invocation details

**Claude Code CLI**

```
claude -p \
  --output-format json \
  --json-schema "$(cat schema/judge.schema.json)" \
  --append-system-prompt-file prompts/judge.system.md \
  --model "${KBX_CLAUDE_MODEL:-haiku}" \
  --max-turns 1 \
  --permission-mode dontAsk --permission-prompts none \
  --setting-sources user \
  < batch.json
```

- Do **not** pass `--bare`: bare mode ignores OAuth login and requires an API key. Instead run with `cwd` set to an empty temp dir so no project `CLAUDE.md`, hooks or `.mcp.json` are loaded, and `--setting-sources user` to skip project settings.
- Structured output lands in `.structured_output`; `total_cost_usd` and usage (including cache reads) are in the same JSON and are stored in `verdicts`.
- Exit code ≠ 0 or missing `structured_output` → batch retried once with half the size, then marked `error`.

**Codex CLI**

```
codex exec --json --output-schema schema/judge.schema.json \
  --ephemeral --skip-git-repo-check --sandbox read-only \
  --model "${KBX_CODEX_MODEL:-gpt-6-luna-fast}" \
  - < batch_with_preamble.md
```

- Codex has no system-prompt flag, so `prompts/judge.system.md` is prepended verbatim as the first bytes of stdin; keep it byte-identical across calls so the provider's prefix cache hits. `turn.completed` events expose `cached_input_tokens`; store it.
- Codex also has `-C <dir>`; use the same empty temp dir.

**Ollama**

`POST /api/chat` with `format: <judge.schema.json>`, `think: false`, `options.temperature: 0`, `keep_alive: "10m"`. Default model `qwen3:14b`; `qwen3:8b` when `kbx doctor` sees < 12 GB free. Context 8K is enough for a 20-pair batch.

**llama-server**

`POST /v1/chat/completions` with `response_format: {type: json_schema, json_schema: …}`; set `--reasoning-budget 0` or the equivalent to disable thinking mode. Same model guidance.

### 6.3 Prompt design (applies to all backends)

```
[STABLE PREFIX — prompts/judge.system.md, hashed into verdicts.prompt_hash]
- Ontology: every class with its description.
- Relation vocabulary: name, definition, direction rule, 2 positive + 1 negative example each.
- Rules: judge only the pair given; prefer `none` over a weak `see_also`; direction is
  informational — the tool decides direction from domain/range; do not infer from vocabulary overlap alone.
- Output contract: free-text `rationale` (≤ 40 words) first, then the clamped fields.

[VARIABLE SUFFIX — one JSON array per batch]
[{"cid":"…","src":{"id","type","title","excerpt(≤600 tok)"},"dst":{…},
  "why_proposed":["adamic_adar:0.42","schema_gap:depends_on","alias_cooccur:'token refresh'"]}, …]
```

`schema/judge.schema.json` (per item): `{cid, rationale, verdict: "accept"|"reject"|"unsure", rel: <vocabulary enum>, confidence: 0..1}`. `unsure` goes to the queue unranked; `confidence` is stored but **not** used for auto-accept (small models' self-reported confidence is uncalibrated).

**Cost controls**

- Batch 10–25 pairs per invocation. CLI start-up and the system prefix dominate per-call cost; batching amortizes both and lets the cache serve the prefix.
- Identical `(cid, backend, model, prompt_hash)` is never re-judged — change the ontology or the prompt and the hash changes, which is the correct invalidation.
- `kbx suggest --budget-usd 0.50` stops when the summed `total_cost_usd` (Claude) or estimated cost from token counts (Codex, from `cached_input_tokens`/`input_tokens` × configured rates) crosses the line. Local backends report `0`.
- `--dry-run` prints the batches and their token estimates without calling anything.

## 7. Review queue and frontmatter writer

```
kbx review                     # interactive TUI: a / r / d / e(dit rel) / s(kip) / q ; shows both excerpts + why_proposed
kbx review --json              # machine-readable queue for editors/agents
kbx accept <cid>… / kbx reject <cid> --reason "…"
```

- Rejections persist in `reviews` with a reason; three rejections of the same `(src,dst)` across any relation suppress the pair from all generators (`strikes`). `kbx unreject` lifts it.
- Acceptance writes `rel: ["[[dst-id]]"]` to the source note's frontmatter **and** the inverse to the target's, unless `ontology.yaml` marks the inverse as derived-only. Writes are atomic (temp file + rename), preserve key order and comments where `serde_yaml` allows, touch nothing outside the frontmatter block, and are refused if the file's `content_hash` changed since the candidate was generated (re-run `suggest`).
- If the target note is not on disk (ACL-hidden, Option A), the pair is never generated in the first place; if it disappears between suggest and accept, acceptance fails with `target.unavailable` and nothing is written.
- Proposals can also be surfaced in-editor: `kbx suggest --stage` writes them under `kbx.proposed` in the source note's frontmatter so Obsidian Properties shows them; `kbx review` reads the same data. `kbx sync-stage` reconciles edits made there (a user deleting a proposal line = reject).

## 8. Author-side validation hook

```
kbx lint [paths…]           # schema + link validation; exit 1 on errors; --format json
kbx hook install            # git pre-commit + optional Obsidian "Shell commands" recipe
kbx watch                   # notify-based; re-compiles changed files, re-runs lint, optionally `suggest` on save with a 45 s debounce
```

Readers' indexes treat every unresolved link as *hidden* (no error). Only the author's lint — run on files the current user modified — reports `link.unresolved`, because only the author can see both ends. This is the one place correctness is enforced under path-based ACLs.

## 9. MCP server (`kbx mcp`)

stdio transport; add to Claude Code with `claude mcp add kbx -- kbx mcp --vault ~/mesh`, to Codex via `codex mcp add`.

| Tool | Args | Returns |
|---|---|---|
| `get_section` | `key` or `id#anchor` | text, heading path, line range |
| `get_linked` | `id`, `rels?`, `depth=1`, `direction=out\|in\|both`, `limit=30` | nodes grouped by relation, weighted, hub-damped |
| `backlinks` | `id`, `rels?` | inbound edges with source sections |
| `path_between` | `a`, `b`, `max_hops=3` | shortest typed paths |
| `search_sections` | `q`, `filter?`, `k=10` | proxies `qmd query --json`; adds graph neighbours (`expand=0\|1`) |
| `suggest_links` | `id?`, `limit`, `judge=true` | runs generators (+ judge) for one note; returns candidates with provenance |
| `review_queue` / `resolve` | — / `cid`, `decision`, `reason?` | lets an agent triage with the human's approval flow preserved (`resolve` is gated by `kbx.toml: mcp.allow_resolve`) |
| `validate` | `id?` | lint findings as advisories |

Read-only by default; `mcp.allow_resolve = true` is the only write path and it still goes through `reviews`.

## 10. CLI surface

```
kbx init [--ontology path] [--search builtin|qmd]   create .kbx/, kbx.toml, example ontology; say which folders were skipped and why; with qmd, detect it and make one collection per scope
kbx index [--full] [paths…]          compile frontmatter → graph.sqlite (incremental by content hash)
kbx lint  [--format json]            validate against ontology
kbx suggest [--note id] [--generators a,b] [--judge auto|claude-cli|codex-cli|ollama|llama-server|none]
            [--batch 20] [--budget N] [--budget-usd X] [--stage] [--dry-run]
kbx review [--json]                  queue UI
kbx accept|reject|defer <cid>… [--reason]
kbx graph  neighbors|path|stats      ad-hoc queries
kbx export edges|reviews|rdf         JSONL / Turtle (via ontology → predicate map)
kbx mcp                              stdio server
kbx watch
kbx doctor [--fix]                   backends, search, models, RAM, index health; on qmd also collections, which --fix creates
```

`kbx.toml`:

```toml
vault = "~/mesh"
scopes = ["team-x", "team-y"]            # top-level folders = ACL scopes
exclude = ["node_modules", ".git", ".obsidian", ".kbx", "credentials", "secrets", ".ssh", ".aws", ".gnupg"]  # see docs/excluding.md
ontology = ".kbx/ontology.yaml"
qmd = { bin = "qmd", collections = ["team-x", "team-y"], roots = { team-x = "team-x", team-y = "team-y" } }

[judge]
order = ["claude-cli", "codex-cli", "ollama", "llama-server"]
batch = 20
claude = { model = "haiku" }
codex  = { model = "gpt-6-luna-fast" }
ollama = { model = "qwen3:14b", host = "http://127.0.0.1:11434" }

[judge.overrides]
"team-y/legal" = "ollama"                # never leaves the machine

[mcp]
allow_resolve = false
```

## 11. npm distribution

Pattern used by esbuild, Biome and Turborepo: a tiny JS package plus one binary package per platform.

- `npm/kbx-cli/package.json`: `"bin": {"kbx": "bin/kbx.js"}`, `"optionalDependencies": {"@kbx/cli-darwin-arm64": "x.y.z", …}` pinned to the same version.
- `bin/kbx.js` maps `process.platform`/`process.arch` (plus glibc vs musl via `detect-libc`) to the package name, resolves the binary with `require.resolve`, and `execFileSync`s it with inherited stdio. Fails with a clear message listing supported targets.
- Platform packages contain only the binary and set `"os"`/`"cpu"` so npm installs exactly one.
- Build and publish with **cargo-dist** (`dist init --installer npm`) or a GitHub Actions matrix (`cross` for Linux arm64/musl); `release.yml` publishes platform packages first, then the shim.
- Also ship `cargo install kbx` and Homebrew via cargo-dist for people without Node. The npm route exists because QMD users already have Node/Bun.

Install: `npm i -g kbx-cli` → `kbx doctor`.

## 12. Hardware and runtime envelope

| Scenario | Resident memory (approx.) |
|---|---|
| `kbx` alone (index + graph) | < 200 MB for 50k notes |
| + QMD models | + 3–4 GB |
| + Ollama `qwen3:8b` judge | + 7 GB → 16 GB machine OK |
| + Ollama `qwen3:14b` judge | + 12 GB → 24 GB GPU / 32 GB Mac |
| Cloud CLI judge | no extra local memory; network + login required |

## 13. Milestones

1. **M1 — compile & lint** (`index`, `lint`, `graph`, `doctor`; ontology.yaml; incremental hashing). No models.
2. **M2 — generators** (`suggest --judge none`; all five generators; `candidates` table; `--dry-run`).
3. **M3 — judge backends** (`claude-cli`, `codex-cli`, `ollama`; detection; batching; cost ledger; `judge.schema.json`).
4. **M4 — review & write** (`review` TUI, `accept/reject`, frontmatter writer with inverses, strikes, `--stage`).
5. **M5 — MCP server** (read tools; `suggest_links`; gated `resolve`).
6. **M6 — packaging** (cargo-dist, npm shim + platform packages, Homebrew, CI matrix).
7. **M7 — learning loop** (`type_pair_prior`; `export reviews`; optional fine-tuned classifier backend).

## 14. Open questions

- **Judge model choice**: run the same 100 labelled pairs through Haiku 5.5, GPT-6 Luna and Qwen3-14B and pick by agreement with human decisions; the spec deliberately does not assume a winner.
- **Section-level candidates**: v0 judges note pairs with best-matching section excerpts; true section→section relations need an anchor convention everyone accepts (`^block-id` is the only portable one).
- **Shared embedding cache**: QMD owns vectors; a per-scope cache would need either a QMD feature or a sidecar reading its `index.sqlite`. Out of scope until measured as a real cost.
- **Codex login check**: no documented `login status` subcommand in the non-interactive docs; the probe approach covers it, but confirm against the installed version.
- **Subscription terms**: using `claude -p` / `codex exec` under a personal subscription for batch judging is supported for scripting, but confirm plan limits and data-retention terms for team content before rollout.
