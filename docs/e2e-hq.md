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
