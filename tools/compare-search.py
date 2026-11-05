#!/usr/bin/env python3
"""Compares what `qmd_knn` found with two backends, from each one's graph.

    tools/compare-search.py overlap <graph A> <graph B>
    tools/compare-search.py topics  <graph> <vault>
    tools/compare-search.py grade   <graph A> <vault A> <graph B> <vault B> [--pairs 100]

Run `kbx suggest --judge none --generators qmd_knn --budget 100000` with each
backend first. `topics` scores a vault from tools/gen-vault.py, whose `tags` name
each note's topic. `grade` has a local model decide, blind, which pairs are
related: nothing leaves the machine. Standard library only. See
docs/search-engine.md.
"""

import argparse
import json
import random
import re
import sqlite3
import urllib.request
from pathlib import Path

JUDGE = "qwen3:8b"
OLLAMA = "http://127.0.0.1:11434/api/generate"
EXCERPT = 1200
PROMPT = """Two notes from one person's knowledge base follow. Would a careful
reader link them, because one is relevant to understanding the other? Sharing a
common word or a generic format is not enough. Answer with one word, yes or no.

Note 1: {a}

Note 2: {b}

/no_think"""


def pairs(graph):
    db = sqlite3.connect(graph)
    query = "SELECT src, dst FROM candidates WHERE generators LIKE '%qmd_knn%'"
    return {tuple(sorted(row)) for row in db.execute(query)}


def paths(graph):
    db = sqlite3.connect(graph)
    return dict(db.execute("SELECT id, path FROM files"))


def overlap(args):
    a, b = pairs(args.a), pairs(args.b)
    union = len(a | b) or 1
    print(f"A {len(a)}, B {len(b)}, both {len(a & b)}, Jaccard {len(a & b) / union:.3f}")


def topic(path):
    found = re.search(r"^tags: \[(.+?)\]", path.read_text(errors="replace"), re.M)
    return found.group(1) if found else None


def topics(args):
    found = pairs(args.graph)
    where = paths(args.graph)
    vault = Path(args.vault)
    of = {note: topic(vault / path) for note, path in where.items()}
    same = sum(of[a] is not None and of[a] == of[b] for a, b in found)
    share = same / len(found) if found else 0
    print(f"{len(found)} pairs, {same} in the same topic, {share:.3f}")


def excerpt(vault, path):
    text = (Path(vault) / path).read_text(errors="replace")
    text = re.sub(r"\A---\n.*?\n---\n", "", text, flags=re.S)
    title = re.search(r"^title: (.+)$", (Path(vault) / path).read_text(errors="replace"), re.M)
    head = title.group(1) if title else ""
    return f"{head}\n{' '.join(text.split())[:EXCERPT]}"


def ask(a, b):
    body = {
        "model": JUDGE,
        "prompt": PROMPT.format(a=a, b=b),
        "stream": False,
        "think": False,
        "options": {"temperature": 0, "seed": 1},
    }
    request = urllib.request.Request(OLLAMA, json.dumps(body).encode())
    with urllib.request.urlopen(request, timeout=300) as response:
        answer = json.load(response)["response"].lower()
    return answer.strip().startswith("yes")


def grade(args):
    rng = random.Random(1)
    sides = {
        "A": (sorted(pairs(args.a)), paths(args.a), args.vault_a),
        "B": (sorted(pairs(args.b)), paths(args.b), args.vault_b),
    }
    work = []
    for side, (found, where, vault) in sides.items():
        for pair in rng.sample(found, min(args.pairs, len(found))):
            work.append((side, pair, where, vault))
    rng.shuffle(work)

    known = {}
    related = {"A": 0, "B": 0}
    asked = {"A": 0, "B": 0}
    for side, pair, where, vault in work:
        if pair not in known:
            first, second = rng.sample(pair, 2)
            known[pair] = ask(excerpt(vault, where[first]), excerpt(vault, where[second]))
        related[side] += known[pair]
        asked[side] += 1
    for side in sides:
        share = related[side] / asked[side] if asked[side] else 0
        print(f"{side}: {related[side]} of {asked[side]} related, {share:.3f}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(required=True)
    command = commands.add_parser("overlap")
    command.add_argument("a")
    command.add_argument("b")
    command.set_defaults(run=overlap)
    command = commands.add_parser("topics")
    command.add_argument("graph")
    command.add_argument("vault")
    command.set_defaults(run=topics)
    command = commands.add_parser("grade")
    command.add_argument("a")
    command.add_argument("vault_a")
    command.add_argument("b")
    command.add_argument("vault_b")
    command.add_argument("--pairs", type=int, default=100)
    command.set_defaults(run=grade)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
