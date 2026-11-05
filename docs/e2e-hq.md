# End-to-end run on the ~/HQ vault

Run on 2026-11-05 with the published `kbx-cli@0.1.3`, on the owner's real
Obsidian vault. No note content from the vault appears here.

**Result: the run stopped at `kbx index`.** kbx installs and its backends are
ready, but it stored 0 of the vault's notes, so there was nothing to suggest,
judge, review or accept. No link was accepted, and AC2 is not met.

| Criterion | State |
| --- | --- |
| AC1 install and `kbx doctor` on this Mac | met |
| AC1 clean-machine install in a Tart VM | untested: the manager's part |
| AC2 relation and inverse in two notes | not met: 0 notes indexed |
| AC3 this record, a task per defect | met |

## Versions

| Tool | Version |
| --- | --- |
| kbx | 0.1.3, from npm as `kbx-cli` |
| qmd | `@tobilu/qmd` 1.0.7, already installed |
| node | 24.14.0 |
| macOS | Darwin 25.3.0, 16 GiB RAM |

## The vault before the run

`~/HQ` is a git repository, but its working tree was **not clean**, so the
precondition for this run wasn't met as written. One note had staged and
unstaged changes from the day before, made by the owner and not by this run.
Stashing or committing it wasn't this run's call, with the vault's sync daemon
reporting itself as running. A whole copy wasn't possible either: the vault is
66 GB and the disk had 17 GiB free. The run went ahead on these safeguards
instead, and left a note on the task saying so:

- `~/HQ.kbx-e2e-backup-2026-11-05` holds the staged and working copies of that
  note, the HEAD commit, the `git status` output and a SHA-256 of all 112,288
  markdown files outside `node_modules` and `.git`. It's 17 MB. The hashes matter
  because git tracks only 2,645 of the vault's markdown files, so a clean tree
  would not have covered the rest.
- `accept` was to touch only tracked, unmodified notes. It never ran.

**Afterwards:** HEAD, `git status` and that note's staged and working copies
were the same, and all 112,288 hashes matched. The run changed no note.

## What ran

| Command | Time | Outcome |
| --- | --- | --- |
| `npm i -g kbx-cli@0.1.3` | 2 s | no earlier global kbx to remove |
| `kbx --version` | | `kbx 0.1.3` |
| `kbx doctor` | 42 s | `claude-cli`, `codex-cli` and `ollama` Ready, qmd present; it installed nothing |
| `kbx init` | 2 m 31 s | `kbx.toml`, `.kbx/ontology.yaml`, 21 qmd collections |
| `kbx index` | 32 s | `indexed 0, 0 unchanged, removed 0, 21728 skipped, 71518 failed, 0 hidden links`, exit 0, 47 MB peak |
| `kbx lint` | 17 s | 71,518 errors, 0 warnings, exit 1 |
| `kbx suggest --judge claude-cli --budget-usd 1.00 --dry-run` | 2 s | `would store 0 candidates` |
| `kbx suggest --judge claude-cli --budget-usd 1.00 --generators adamic_adar` | 0.2 s | 0 candidates, 0 judged |
| `kbx review --json` | | `[]` |

The ontology stayed as generated: the failures come from the parser, before any
class or relation is looked at, so changing it wouldn't have helped.

The plain `kbx suggest` from the plan didn't run. It would have started `qmd
embed` on the whole vault with nothing to gain, so the real run named a generator
that doesn't use qmd. The budget wasn't raised, because there was nothing to
judge.

**Cost:** $0.00. No judge was called and no note excerpt left the machine.

## Why nothing was indexed

kbx needs `id`, `type` and `title` in a note, and refuses a note if any key
other than `kbx:` or `qmd:` holds a nested map.

| Markdown files the index walked | Count |
| --- | --- |
| No frontmatter, skipped | 21,728 |
| Nested map under a key of the vault's own | 45,512 |
| Missing `id`, `type` or `title` | 25,760 |
| Other: invalid YAML, bad UTF-8, unclosed frontmatter | 246 |

The log gives only the first problem in each note, so a separate count of the
frontmatter keys checked it. Of 71,513 notes with frontmatter, 46,862 have `id`,
47,943 have `type` and 20,296 have `title`. **None has all three**, tracked notes
included, so fixing the nested-map rule alone would still store nothing.

Getting past this on `~/HQ` today would mean hand-adding keys to the owner's
notes. That wasn't part of this run and it wasn't done.

## Not exercised

Because of the above, this run says nothing about these on a real vault:
candidate generation, the `claude-cli` judge, its spend, the budget cap, `kbx
accept`, the frontmatter writer and embedding time.

## Defects

| Task | Defect |
| --- | --- |
| `a27b71fc-1c30-4688-8b53-32a0979a812d` | An existing vault has no way to adopt kbx. **Blocks AC2.** |
| `88608745-c533-4b9e-bd15-3887f96bbf3f` | Nested maps under unknown keys fail the whole note |
| `f308a61f-dd2e-4393-acb7-36e5a0aa9406` | `index`, `doctor` and `suggest` report success on an empty graph |
| `b168bcf5-bdef-4729-9d1d-6b92b2f6dc3c` | Generated scopes include `credentials` and `node_modules`, and `.gitignore` isn't read |
| `63c34f55-6274-4226-b0ed-88c6812a35ea` | `suggest` refreshes qmd even when the graph is empty |

## Other surprises

- `credentials` became a scope. It held 8 files and no markdown, so nothing from
  it was indexed, but the same folder holding `.md` files would have been.
- `doctor` shows `ollama` as `qwen3:8b` where `kbx.toml` says `qwen3:14b`. That
  isn't a defect: the backend steps down when free memory is short.
- The task's criteria still say `npm i -g kbx`. The package is now `kbx-cli`.

## What the run left behind

- In `~/HQ`: `kbx.toml` and `.kbx/`. `.git/info/exclude` keeps both out of git.
- `~/.cache/qmd/kbx-16d3d62c1a97.sqlite` and its `.yml` in `~/.config/qmd/`: 1.3 GB,
  with the vault's text in it. The owner's own qmd index wasn't touched.
- `~/HQ.kbx-e2e-backup-2026-11-05`.
- `kbx-cli` installed globally.

They're kept for the re-run once adoption is fixed. To remove the disk usage,
delete those paths.

## Second run

Run on 2026-11-06 with the published `kbx-cli@0.1.4`, under the owner's rule that
nothing in `~/HQ` may change except `~/HQ/.kbx`. Frontmatter writing stayed off.

**Result:** kbx now works on the vault. It indexed 70,942 notes, a judge reviewed
1,620 candidates for $0.99, and three relations were accepted into `.kbx` with
their diffs shown. No note changed. Two things fall short of the plan:

- **The before and after hashes do not match.** No note is among the differences,
  and the evidence points to other sessions, not kbx, but the rule said they must
  match. See [the hash check](#the-hash-check).
- **The full generator set didn't run.** `qmd_knn` needs an embed that would take
  about 3.6 hours, and the structural generators found nothing, so every candidate
  came from `alias_cooccur`.

| Criterion | State |
| --- | --- |
| AC1 install and `kbx doctor` on this Mac | met |
| AC2 relation in `.kbx`, diff shown | met |
| AC2 no file outside `.kbx` changes | not met as written: for the manager to rule on |
| AC3 this record, a task per defect | met |

### Second run versions

| Tool | Version |
| --- | --- |
| kbx | 0.1.4, from npm as `kbx-cli` |
| qmd | `@tobilu/qmd` 1.0.7 |
| Judge | `claude-cli`, model `haiku`, chosen by `--judge auto` |

### Second run commands

| Command | Time | Outcome |
| --- | --- | --- |
| Hash of `~/HQ` outside `.kbx` and `.git` | 88 s | 727,816 entries, 0 errors |
| `kbx init --search qmd` | 20 s | `.kbx/kbx.toml` written; the `companies` collection failed |
| `kbx doctor` | 1 s | config at `.kbx/kbx.toml`, `claude-cli` Ready, search `qmd` |
| `kbx index` | 28.5 s | see below, 196 MB peak |
| `kbx lint` | 18.5 s | 214,390 errors, 1 warning, exit 1 |
| `kbx suggest --judge auto --budget-usd 1.00 --dry-run` | 6 m 24 s | read-only, 100 batches |
| `kbx doctor --fix` | 6 s | the same `companies` failure, exit 0 |
| `qmd collection add` for `companies`, by hand | 2 m 24 s | 80,050 documents in the index |
| `qmd embed` | stopped at 6 m | 256,317 chunks, about 20 a second, about 3.6 hours |
| `kbx suggest --judge auto --budget-usd 1.00 --generators …` | 48 m 44 s | see below |
| `kbx accept`, three times | under 1 s each | exit 0, each `not written` |
| `kbx suggest --judge none --generators …`, structural only | 2 s | 0 candidates |

`kbx doctor` has no line for the write setting, so that was confirmed another way:
`.kbx/kbx.toml` has no `[write]` table, and each accept printed `not written`.

### Index

```
indexed 70942, 0 unchanged, removed 0, 8444 skipped, 142 failed, 104504 hidden links, 70942 inferred
left out 8586 files, by cause:
  8444  id.duplicate: a note at a lower path has the same `id`
   135  note.invalid: frontmatter is not valid YAML
     3  note.invalid: frontmatter has no closing `---`
     3  note.invalid: the file can't be read as UTF-8 text
     1  nested map: frontmatter key `tags` holds a nested map
```

Every stored note had something inferred, as the first run's counts predicted.
Most lint errors are `link.unresolved` (about 152,800), `type.unknown` (about
47,200) and `id.duplicate` (about 14,200). The vault's own `type` values number
147, and `kbx doctor` lists them; the ontology wasn't extended for them.

### Exclusions

`.gitignore` removes **11,624 markdown files**, against the exclusions PR's
estimate of 11,193. That's the 91,152 files visible once hidden folders and the
default `exclude` list are out, less 70,942 stored and 8,586 left out by cause.

| Top-level folder | Markdown files removed | Why |
| --- | --- | --- |
| `workspace` | 8,158, all of it | ignored; `kbx init` says so |
| `personal` | 3,027, all of it | `personal/.gitignore` starts with `/*`; `kbx init` says nothing |
| `companies` and the rest | about 440 | individual ignored paths |
| `repos` | none | ignored, but holds no markdown |

`personal` is the one for the owner's include-list decision: git tracks 2,645 of
those notes, force-added past that rule, and they're the kind of notes kbx is for.

### Search

`kbx init --search qmd` couldn't reuse the first run's index as planned:

- That index had 0 vectors, because the first run never embedded. Reuse would have
  saved the text scan only.
- The mask for `companies` came to 2,139 globs and 103,414 characters, and qmd stops
  at 10,000. `init` had already removed the old collection, so the index fell from
  91,759 documents to 9,526.

The manual repair touched only qmd's index for this vault, outside `~/HQ`: it
removed the leftover `workspace`, `node-modules`, `credentials` and `repos`
collections and added `companies` back with `**/*.md`. So about 440 ignored notes
are in that local index again. They have no row in the graph, so they can't reach
a judge. The embed was stopped under the run's 90-minute limit, with 2,400 chunks
done, and `qmd_knn` didn't run. That is why `suggest` named its generators:
`schema_gap`, `alias_cooccur`, `common_neighbors`, `adamic_adar`, `two_hop_bridge`
and `type_pair_prior`.

### Candidates and cost

```
2000 candidates
  alias_cooccur 2000
dropped 0 linked or rejected, 10424689 over budget
judged by claude-cli (haiku): 213 accept, 1248 reject, 159 unsure, 360 unjudged, 20 error, 0 already judged
  $0.9924, 408726 cached tokens
budget reached: spent $0.9924 of $1.0000; run again to judge the 360 left
```

The budget held. The judge returned a malformed batch 10 times, and two of those, after retries, cost 20 pairs.
Those judged were excerpts of notes under `companies/`, which is where all 2,000
candidates were.

**Inline links:** no inline-only candidate exists to review, so `mention_weight`
got no test on this vault. `alias_cooccur` filled the budget, and a separate run of
the three structural generators alone found 0 candidates. The cause is upstream of
them: only 5 of 104,509 links resolve. 104,446 of the rest are partial paths like
`[[folder/name]]`, and kbx matches a bare file name or a full path only. 71,660 of
them end exactly one stored path, so they could resolve.

**Quality of what was accepted**, from reading the top 30 of 213:

- All 30 score 1.0, so the order says little.
- Several `part_of` pairs read backwards, a whole as part of its component.
- Many come from a backup folder that repeats live notes, so the owner may want it
  in `exclude`.

### Accepted relations

Each was chosen for being clearly right from its titles. Pairs involving people and
those that read backwards were passed over, so these aren't simply the top three.

| Candidate | Relation |
| --- | --- |
| `1f76d894e5e2b120a42c1365055ce7a7` | `sync-now` `part_of` `hq-sync` |
| `c59ff8e434672dc46c1f5c848f5a260c` | `slack-bot-factory` project `see_also` concept |
| `4fd8b85bbdee6178919af74b7d06f9f9` | `indigo` company note `see_also` `INDEX` |

The diffs `kbx accept` printed, with their unchanged context lines removed because
those include note text:

```diff
--- a/companies/indigo/ontology/entities/concept/sync-now.md
+++ b/companies/indigo/ontology/entities/concept/sync-now.md
@@ -62,6 +62,7 @@
+part_of: ["[[53d401c45e45cdad1e85515bf52e5b89c7b7f20bb351b780b9f2237d162eb1a4]]"]
--- a/companies/indigo/ontology/entities/project/hq-sync.md
+++ b/companies/indigo/ontology/entities/project/hq-sync.md
@@ -40,6 +40,7 @@
+has_part: ["[[03418c388c34b523be07dd0fd4774ed9e59053d92d6b1c53e66a608528aa47eb]]"]

--- a/companies/indigo/ontology/entities/concept/slack-bot-factory.md
+++ b/companies/indigo/ontology/entities/concept/slack-bot-factory.md
@@ -49,6 +49,7 @@
+see_also: ["[[898e443cc3e9f8c1844927b0164535562721e4bb596c4d50e837fb42792f07f7]]"]
--- a/companies/indigo/ontology/entities/project/slack-bot-factory.md
+++ b/companies/indigo/ontology/entities/project/slack-bot-factory.md
@@ -61,6 +61,7 @@
+see_also: ["[[b48d09178013dc6c3a4dee0541d9fe8cea801c9b2a93c66511493255f1b9db50]]"]

--- a/companies/indigo/INDEX.md
+++ b/companies/indigo/INDEX.md
@@ -1,3 +1,6 @@
+---
+see_also: ["[[2a7694062c7e7af362fdf4c9b40181307b02b06c147a7df3082a890bcf37f156]]"]
+---
--- a/companies/indigo/ontology/entities/company/indigo.md
+++ b/companies/indigo/ontology/entities/company/indigo.md
@@ -96,6 +96,7 @@
+see_also: ["[[companies/indigo/INDEX]]"]
```

`kbx review --accepted` lists all three, `kbx export edges` emits them with origin
`kbx`, and `kbx doctor` went from 4 edges to 10, the three and their inverses. Nothing
was written: those six files are unchanged by hash.

### The hash check

SHA-256 of every file and symlink target under `~/HQ`, outside the top-level `.kbx`
and `.git`, taken before anything else ran and again after everything else.

| | Entries |
| --- | --- |
| Before, 01:49 | 727,816 |
| After, 03:02 | 727,860 |
| Added | 112 |
| Removed | 68 |
| Content changed | 16 |
| Same content, new mtime | 8 |
| Of those, `.md` files | **0** |

**The two do not match.** `git status` and HEAD were the same, and none of the
changed files is tracked. The differences are all session state in `workspace/`
(such as `orchestrator`, `sessions` and `work-mesh`), `.claude/state`, and one file
in `.hq/usable-integrations`.

The evidence says these came from other sessions and not from kbx:

- Every change that has a timestamp falls between 02:08:31 and 02:20:51.
- The first came 92 seconds before `kbx suggest` started at 02:10:03, when kbx wasn't
  running at all.
- None falls in the 38 minutes of judging after 02:20:51, or at any accept.
- The files sit under per-session folders such as `workspace/sessions/01a1ad8a-…`,
  which belong to five short sessions, none of them this run's.
- The judge runs in a temporary folder with user settings only, and no user settings
  file on this machine has a hook that touches `~/HQ`.
- No changed path has `kbx` in it, and none is a `.tmp` file.

Two limits apply. The 68 removals, all in `.claude/state`, have no timestamp to
check. And a control hash after the run, with nothing of kbx's running, showed no
change at all, so it doesn't show that the tree moves by itself, only that it was
quiet then. This run can't say which process wrote the files.

For a clean result, repeat the hash with no other session running in `~/HQ`.

### Second run defects

| Task | Defect |
| --- | --- |
| `772147d5-92ad-4201-8b12-6dd37d0982bf` | Partial-path wikilinks don't resolve, so the structural generators have no graph. **Blocks the inline-link check.** |
| `a9f7e317-09aa-4d71-9bef-d84bb4cc3c7d` | `suggest` blocks on a whole-vault embed, about 3.6 hours here. **Blocks `qmd_knn`.** |
| `c7bcd123-1d1b-4be9-b0ae-a20e9ef63e52` | qmd masks can pass its length limit, a failed add loses the collection, and `doctor --fix` exits 0 |
| `c0be3ac8-6688-47ec-a86a-0cb73133daf0` | `alias_cooccur` can take the whole candidate budget, and top scores saturate |
| `25bf9b7a-9636-4079-94e4-b7c0257ec0d9` | A folder whose contents are all ignored goes unreported, and tracked notes count as ignored |
| `f0463cee-ac97-46cb-b9c3-65b7e3825393` | `part_of` proposals can read backwards |
| `a960dbde-a2ba-4b95-ad1b-363da8825f55` | qmd collections left over from an earlier config are neither reported nor removed |
| `6c65c2ee-4815-461b-8a79-f303e8189be3` | `kbx doctor` doesn't show the write setting |
| `c29f8c7a-c210-4055-82a6-44f9dafc9645` | An ontology from 0.1.3 has no `Note` class, and `init` keeps it |
| `d37b8116-1174-4161-abed-866bb64f5534` | The `search` comment `kbx init` writes names the wrong alternative |

### What the second run left behind

- In `~/HQ/.kbx`: the config, the 160 MB graph with the three relations, and the
  ontology, which gained one line by hand for the `Note` class.
- `~/.cache/qmd/kbx-16d3d62c1a97.sqlite` and `~/.config/qmd/kbx-16d3d62c1a97.yml`,
  with the vault's text in them and a partial embed that `qmd embed` can resume.
- `~/HQ.kbx-e2e-run2-2026-11-06`: the manifests, logs and `.kbx` as it stood before.
- `kbx-cli` 0.1.4 installed globally.
