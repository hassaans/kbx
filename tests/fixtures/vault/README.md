# Fixture vault

The small vault every kbx test runs against. Use it with
`schema/ontology.example.yaml`. `crates/kbx-core/tests/fixture_vault.rs` checks
everything below, so update both together.

Note ids are `01KBX0000000000000000000NN` and filenames are `{id}-{slug}.md`.
Each top-level folder is a scope. This README has no frontmatter, so
`parse_note` returns `ParseError::NoFrontmatter` for it, the same as for any
plain markdown file in a real vault. Walkers should skip it.

## Finding codes

| Code | Note | Why |
|---|---|---|
| `type.unknown` | `lint/…17-type-unknown` | `type: Recipe` |
| `rel.unknown` | `lint/…18-rel-unknown` | `relates_to` key |
| `rel.range` | `lint/…19-rel-range` | `implements` points at a Decision |
| `rel.domain` | `lint/…24-rel-domain` | `depends_on` on a Concept |
| `schema.gap` | `operations/…10-restart-service` | Procedure without `depends_on` |
| `rel.cardinality` | `lint/…20-rel-cardinality` | `supersedes` has two targets |
| `link.unstable` | `lint/…21-link-unstable` | links to `legacy-runbook.md` by filename |
| `link.unresolved` | `lint/…23-link-unresolved` | twice: a frontmatter link and an inline `[[ghost note]]` |
| `note.invalid` | `lint/…25-bad-yaml`, `lint/…26-unterminated` | YAML that doesn't parse, and frontmatter that never closes |
| `id.duplicate` | `lint/…27-id-duplicate-a`, `lint/…27-id-duplicate-b` | both claim id `…27`; `-a` is the lower path, so it keeps the id |

No other note has a finding. `lint/legacy-runbook.md` (id `…22`) is the one file
without an id prefix; it's the target of the `link.unstable` case.

`kbx index` compiles 25 of the 28 files with frontmatter. It fails the two
`note.invalid` files and skips `…27-id-duplicate-b`.

## §5 generator cases

| Generator | Notes | Case |
|---|---|---|
| `common_neighbors`, `adamic_adar` | `…08-deploy-service`, `…09-rotate-keys` | unlinked, and both depend on `…06-rate-limits` and `…07-retries` |
| `two_hop_bridge` | the same pair | A→X←B through either shared Concept |
| `schema_gap` | `…10-restart-service` | Procedure without `depends_on`; `operations/` has candidates in the relation's range |
| `alias_cooccur` | `…11-incident-response` | body says "token bucket", an alias of `…06-rate-limits`, with no link |
| `qmd_knn` | `…12-session-expiry`, `…13-cookie-lifetime` | same topic and vocabulary, no link, no shared alias |
| `type_pair_prior` | whole vault | learned from `reviews` rows, not from any one note; the vault supplies varied class pairs |
| hub damping | `…01-platform` | the highest-degree node by a wide margin, linked by `part_of` and `see_also` from three scopes |

## Parsing cases

- `engineering/…02-oauth-flow` has the full §3 frontmatter, including `kbx:` and
  `qmd:`, plus a preamble, duplicate `## Steps` headings, block ids on a list
  item, a paragraph and a table, and a fenced block that must be ignored.
- `engineering/…03-tokens` and `…04-redirects` carry the inverse key
  `required_by`; `…05-auth-overview` carries `has_part`.
- `engineering/…07-retries` has a link that resolves by alias.
