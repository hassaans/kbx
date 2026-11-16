#!/usr/bin/env python3
"""A synthetic vault for `alias_cooccur`, with labels: the same bytes for the
same arguments.

    tools/gen-alias-vault.py <dir> [--notes 10000] [--seed 7]

Writes notes to `<dir>/vault` and the pairs it knows the truth of to
`<dir>/labels.jsonl`, in the format `kbx import reviews` reads.

- **Narrow terms.** One note in 20 is a topic with a two-word alias, such as
  "quartz ledger". Two to six other notes are about it: they say the alias and
  are `part_of` a shared group note. Each of those pairs is accepted: topic and
  member by the alias, and two members by their group.
- **Broad terms.** Three projects with one-word names, Atlas, Orion and Nova,
  are said in passing, one to four times, by 20%, 8% and 3% of the notes.
  Fifteen notes are about each and say it three times; those pairs are
  accepted, the passing ones rejected.
- **Common titles.** Thirty notes are titled Overview, Summary, Status,
  Planning, Meeting or Update, five to a title, and most bodies use one or two
  of those words in prose, up to three times. Those pairs are rejected.

The rest of each body is filler that names nothing. See `docs/generators.md`.
"""

import argparse
import json
import random
from pathlib import Path

CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
ADJECTIVES = (
    "quartz amber cobalt velvet granite copper silent hollow crimson frozen "
    "golden ivory jade lunar marble nimble opal pewter russet sable tawny umber "
    "violet willow azure brass cedar dusky ember flint garnet hazel indigo"
).split()
NOUNS = (
    "ledger beacon lantern compass harbor anvil bridge canopy citadel delta "
    "furnace galley hangar island jetty kiln lattice meadow orchard pavilion "
    "quarry rampart spindle turbine vault wharf yardarm zephyr bastion crucible"
).split()
PROJECTS = [("Atlas", 0.20), ("Orion", 0.08), ("Nova", 0.03)]
COMMON = ["Overview", "Summary", "Status", "Planning", "Meeting", "Update"]
FILLER = (
    "the a of to and in is that for it with as was on be by this are from at "
    "which but have had they you were their one all we can her has there been "
    "if more when will would who so no she other its may these about over such "
    "through most into than only some time could them two then do first any my "
    "now like our between both after those where much should well people just "
    "because how too little very still own while last might us great old year "
    "off come since against go came right used take three states himself few "
    "house use during without again place around however home small found mrs "
    "thought went say part once general high upon school every don does got "
    "left number course war until always away something fact though water less"
).split()
ABOUT = "This note is about the {}. The {} matters here, and the {} again."


def ulid(rng):
    return "01KBX" + "".join(rng.choice(CROCKFORD) for _ in range(21))


def filler(rng, words):
    return " ".join(rng.choice(FILLER) for _ in range(words)).capitalize() + "."


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("dir", type=Path)
    parser.add_argument("--notes", type=int, default=10000)
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args()
    rng = random.Random(args.seed)
    vault = args.dir / "vault"

    notes = [
        {"id": ulid(rng), "title": f"Record {n}", "aliases": [], "says": [], "part_of": None}
        for n in range(args.notes)
    ]
    order = list(range(args.notes))
    rng.shuffle(order)
    taken = iter(order)
    labels = []

    def label(a, b, decision):
        rel = "see_also" if decision == "accept" else "none"
        labels.append((notes[a]["id"], notes[b]["id"], rel, decision))

    for title in COMMON:
        for _ in range(5):
            notes[next(taken)]["title"] = title
    common = [n for n, note in enumerate(notes) if note["title"] in COMMON]

    for name, share in PROJECTS:
        project = next(taken)
        notes[project]["title"] = name
        about = [next(taken) for _ in range(15)]
        for n in about:
            notes[n]["says"] += [ABOUT.format(name, name, name)]
            label(n, project, "accept")
        for n in rng.sample(range(args.notes), int(share * args.notes)):
            if n != project and n not in about:
                notes[n]["says"] += [f"As with {name}, this moved on."] * rng.randint(1, 4)
                label(n, project, "reject")

    pairs = rng.sample([(a, b) for a in ADJECTIVES for b in NOUNS], len(ADJECTIVES) * len(NOUNS))
    for k in range(min(args.notes // 20, len(pairs))):
        try:
            topic = next(taken)
            group = next(taken)
            members = [next(taken) for _ in range(rng.randint(2, 6))]
        except StopIteration:
            break
        alias = " ".join(pairs[k])
        notes[topic]["aliases"].append(alias)
        notes[group]["title"] = f"Group {k}"
        for m in members:
            times = rng.randint(1, 3)
            notes[m]["says"] += [f"It turns on the {alias}."] * times
            notes[m]["part_of"] = notes[group]["id"]
            label(m, topic, "accept")
        for i, a in enumerate(members):
            for b in members[i + 1 :]:
                label(a, b, "accept")

    for n, note in enumerate(notes):
        if rng.random() < 0.6:
            for word in rng.sample(COMMON, rng.randint(1, 2)):
                note["says"] += [f"{word} of the week came and went."] * rng.randint(1, 3)
                for c in common:
                    if notes[c]["title"] == word and c != n:
                        label(n, c, "reject")

    vault.mkdir(parents=True, exist_ok=True)
    for note in notes:
        lines = ["---", f"id: {note['id']}", "type: Concept", f"title: {note['title']}"]
        if note["aliases"]:
            lines.append(f"aliases: [{', '.join(note['aliases'])}]")
        if note["part_of"]:
            lines.append(f'part_of: ["[[{note["part_of"]}]]"]')
        lines.append("---")
        parts = [filler(rng, rng.randint(40, 120)) for _ in range(rng.randint(1, 3))]
        for said in note["says"]:
            parts.insert(rng.randint(0, len(parts)), said)
        text = "\n".join(lines) + "\n" + "\n\n".join(parts) + "\n"
        (vault / f"{note['id']}.md").write_text(text)

    seen = set()
    with open(args.dir / "labels.jsonl", "w") as out:
        for at, (src, dst, rel, decision) in enumerate(labels):
            key = tuple(sorted((src, dst)))
            if key in seen:
                continue
            seen.add(key)
            row = {"src": src, "dst": dst, "rel": rel, "decision": decision,
                   "reason": None, "strikes": 0, "decided_at": 1700000000 + at}
            out.write(json.dumps(row) + "\n")
    accepted = sum(1 for _, _, _, d in labels if d == "accept")
    print(f"{len(notes)} notes, {len(seen)} labelled pairs, {accepted} accepted")


if __name__ == "__main__":
    main()
