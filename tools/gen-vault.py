#!/usr/bin/env python3
"""A synthetic kbx vault, the same bytes for the same arguments.

    tools/gen-vault.py gen ~/Library/Caches/kbx-bench/vault
    tools/gen-vault.py edit ~/Library/Caches/kbx-bench/vault --count 10

`gen` writes `--notes` notes of 100 to 800 words over the scopes in `SCOPES`,
each with a few headings, most with aliases, relations and inline wikilinks,
and some with no link at all, which are the notes `qmd_knn` searches first.
`edit` changes one section of `--count` notes spread over the vault. Each
prints a SHA-256 over every note's path and bytes, to compare runs by.

Standard library only. See docs/embedding-cache.md for what this is for.
"""

import argparse
import hashlib
import random
import re
import sys
from pathlib import Path

CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

# Each scope's topics, and the terms its notes are written from. A note keeps
# to one topic, so that neighbours exist and are not the whole scope.
SCOPES = {
    "engineering": {
        "authentication": "oauth flow, refresh token, session cookie, password hash, single sign-on, access scope, token rotation, identity provider",
        "storage": "write-ahead log, compaction, replica lag, partition key, secondary index, snapshot isolation, vacuum job, page cache",
        "networking": "load balancer, connection pool, retry budget, circuit breaker, service mesh, tls handshake, dns failover, rate limit",
        "deployment": "canary release, rollback plan, feature flag, blue-green switch, container image, build cache, release train, health probe",
        "observability": "trace span, error budget, log sampling, metric cardinality, alert threshold, latency histogram, dashboard panel, on-call rotation",
        "frontend": "hydration mismatch, bundle size, design token, render loop, form validation, keyboard focus, service worker, layout shift",
    },
    "operations": {
        "incidents": "incident commander, severity level, status page, postmortem review, customer impact, mitigation step, timeline note, paging policy",
        "procurement": "purchase order, vendor review, renewal date, spend limit, contract redline, approval chain, invoice match, budget owner",
        "facilities": "badge access, desk booking, visitor policy, fire warden, lease renewal, cleaning schedule, meeting room, shipping dock",
        "security": "phishing drill, access review, laptop encryption, vulnerability scan, audit evidence, vendor questionnaire, key custody, breach notice",
        "support": "ticket queue, escalation path, macro reply, first response, refund request, customer tier, handoff note, satisfaction score",
    },
    "research": {
        "retrieval": "dense embedding, reranker score, query expansion, recall curve, chunk overlap, hybrid fusion, relevance label, vector quantization",
        "evaluation": "held-out split, inter-rater agreement, confidence interval, ablation study, baseline model, error taxonomy, sample size, test leakage",
        "language models": "context window, instruction tuning, sampling temperature, tokenizer drift, distilled student, reward model, prompt template, attention head",
        "user studies": "think-aloud session, consent form, diary study, task completion, recruiting screener, affinity map, interview guide, usability score",
        "knowledge graphs": "entity linking, relation schema, typed edge, ontology class, graph traversal, link prediction, inverse relation, node degree",
    },
    "product": {
        "pricing": "seat license, usage tier, annual discount, overage charge, free trial, price anchor, packaging change, enterprise quote",
        "onboarding": "activation milestone, empty state, sample project, invite flow, setup checklist, welcome email, first session, drop-off step",
        "roadmap": "quarterly bet, opportunity score, dependency map, launch gate, scope cut, success metric, stakeholder review, discovery track",
        "growth": "referral loop, retention cohort, conversion funnel, pricing page, churn signal, experiment guardrail, lifecycle email, attribution model",
        "mobile": "push notification, offline mode, app review, deep link, crash report, background sync, gesture hint, release note",
    },
    "people": {
        "hiring": "job scorecard, sourcing channel, phone screen, work sample, interview loop, offer band, reference check, candidate debrief",
        "performance": "growth plan, calibration session, peer feedback, promotion packet, career ladder, one-on-one agenda, goal review, skill matrix",
        "benefits": "health plan, parental leave, pension match, learning stipend, sick policy, equity refresher, open enrollment, relocation support",
        "culture": "all-hands format, team ritual, remote norm, recognition note, values review, mentoring circle, working agreement, offsite plan",
    },
}

TYPES = ["Concept"] * 5 + ["Procedure"] * 3 + ["Decision"] * 2 + ["Reference"] * 2

OPENINGS = [
    "This note covers {a} and how it relates to {b}.",
    "We keep coming back to {a} whenever {b} changes.",
    "The short version: {a} is what {b} depends on in practice.",
    "Most questions about {a} turn out to be questions about {b}.",
    "Written after the last review of {a}, with {b} in mind.",
]

SENTENCES = [
    "The team measured {a} for a month before touching {b}.",
    "If {a} drifts, {b} is usually the first thing to look at.",
    "Nobody owns {a} on paper, which is why {b} gets missed.",
    "A change to {a} needs a second reader, because {b} hides the effect.",
    "We tried replacing {a} with a simpler {b}, and it held up for small cases only.",
    "The failure we saw last quarter came from {a} running ahead of {b}.",
    "Keep {a} boring: the interesting work happens around {b}.",
    "When people say {a} they often mean {b}, so ask which one before planning.",
    "The cost of {a} grows with every {b} added to the process.",
    "There is an older write-up of {a} that disagrees about {b}; it predates the current setup.",
    "For now {a} stays manual, since automating it would make {b} harder to audit.",
    "The numbers for {a} look fine until {b} is broken down by team.",
    "A good first check is whether {a} still matches what {b} assumes.",
    "Expect {a} to take longer than the estimate whenever {b} is involved.",
    "We review {a} every six weeks and write down what changed in {b}.",
    "One open question is how far {a} should go before {b} has to change too.",
    "Treat {a} as a default, and record any exception next to {b}.",
    "The checklist for {a} has three steps, and the last one is confirming {b}.",
]

HEADINGS = [
    "Background", "Context", "How it works", "Steps", "Why this way", "Trade-offs",
    "Known problems", "Open questions", "What we tried", "Who to ask", "Next review",
    "Examples", "Rules of thumb", "History", "Measurements", "Risks",
]


def ulid(rng):
    """A ULID-shaped id: the shared prefix of the test fixtures, then noise."""
    return "01KBX" + "".join(rng.choice(CROCKFORD) for _ in range(21))


def slug(title):
    return re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")


def words_wanted(rng):
    """100 to 800 words, skewed short the way a real vault is."""
    return min(800, max(100, int(rng.lognormvariate(5.7, 0.55))))


def plan(args):
    """Who each note is, before any of them is written, so links can point
    forward as well as back."""
    rng = random.Random(args.seed)
    scopes = list(SCOPES)[: args.scopes]
    notes = []
    for n in range(args.notes):
        scope = scopes[n % len(scopes)]
        topic = rng.choice(list(SCOPES[scope]))
        terms = [t.strip() for t in SCOPES[scope][topic].split(",")]
        first, second = rng.sample(terms, 2)
        kind = rng.choice(TYPES)
        title = f"{first.capitalize()} and {second} {rng.choice(['notes', 'guide', 'review', 'overview', 'playbook', 'log'])} {n}"
        note = {
            "id": ulid(rng),
            "scope": scope,
            "topic": topic,
            "terms": terms,
            "kind": kind,
            "title": title,
            "aliases": [],
        }
        note["file"] = f"{note['id']}-{slug(title)}"
        notes.append(note)
    return notes


def fill(rng, template, terms):
    a, b = rng.sample(terms, 2)
    return template.format(a=a, b=b)


def body(rng, note, peers):
    """Sections of paragraphs until the note has its words, with a wikilink to
    a peer now and then."""
    want = words_wanted(rng)
    terms = note["terms"]
    headings = rng.sample(HEADINGS, rng.randint(2, 5))
    per_section = want // (len(headings) + 1)
    linking = rng.random() < 0.35
    parts = [fill(rng, rng.choice(OPENINGS), terms)]
    for heading in [None] + headings:
        if heading:
            parts.append(f"## {heading}")
        written = 0
        while written < per_section:
            count = rng.randint(2, 5)
            sentences = [fill(rng, rng.choice(SENTENCES), terms) for _ in range(count)]
            if linking and rng.random() < 0.15:
                sentences.append(f"See [[{rng.choice(peers)['file']}]] for more.")
            paragraph = " ".join(sentences)
            written += len(paragraph.split())
            if written > per_section and len(parts) > 1 and parts[-1][0] != "#":
                break
            parts.append(paragraph)
    return "\n\n".join(parts)


def frontmatter(rng, note, hubs, peers):
    lines = ["---", f"id: {note['id']}", f"type: {note['kind']}", f"title: {note['title']}"]
    alias_count = rng.choice([0, 1, 1, 2, 3])
    if alias_count:
        n = note["title"].rsplit(" ", 1)[1]
        aliases = [f"{term} {n}" for term in rng.sample(note["terms"], alias_count)]
        lines.append(f"aliases: [{', '.join(aliases)}]")
    lines.append(f"tags: [{slug(note['topic'])}]")

    def link(target):
        return f'"[[{target["file"]}]]"'

    # About a fifth have no relation, and those are what qmd_knn is for.
    if rng.random() < 0.8:
        lines.append(f"part_of: [{link(rng.choice(hubs))}]")
    if note["kind"] == "Procedure" and rng.random() < 0.85:
        targets = [p for p in rng.sample(peers, 6) if p["kind"] in ("Concept", "Procedure")]
        if targets:
            lines.append(f"depends_on: [{', '.join(link(t) for t in targets[:3])}]")
    if rng.random() < 0.3:
        lines.append(f"see_also: [{link(rng.choice(peers))}]")
    lines.append("---")
    return "\n".join(lines)


def digest(vault):
    sha = hashlib.sha256()
    for path in sorted(vault.rglob("*.md")):
        sha.update(path.relative_to(vault).as_posix().encode())
        sha.update(b"\0")
        sha.update(path.read_bytes())
    return sha.hexdigest()


def gen(args):
    vault = Path(args.vault).expanduser()
    if vault.exists() and any(vault.iterdir()):
        sys.exit(f"{vault} is not empty")
    notes = plan(args)
    by_topic = {}
    for note in notes:
        by_topic.setdefault((note["scope"], note["topic"]), []).append(note)
    rng = random.Random(args.seed + 1)
    words = 0
    for note in notes:
        peers = by_topic[(note["scope"], note["topic"])]
        peers = [p for p in peers if p is not note] or peers
        # The first few notes of a topic are its hubs.
        hubs = [p for p in peers[:5]]
        head = frontmatter(rng, note, hubs, peers if len(peers) >= 6 else peers * 6)
        text = body(rng, note, peers)
        words += len(text.split())
        folder = vault / note["scope"]
        folder.mkdir(parents=True, exist_ok=True)
        (folder / f"{note['file']}.md").write_text(f"{head}\n{text}\n", encoding="utf-8")
    print(f"{len(notes)} notes, {words} body words, in {len(by_topic)} topics")
    print(f"sha256 {digest(vault)}")


def edit(args):
    """Rewrites the last paragraph of `--count` evenly spaced notes. `--round`
    makes a further edit differ from the last."""
    vault = Path(args.vault).expanduser()
    paths = sorted(vault.rglob("*.md"))
    stride = max(1, len(paths) // args.count)
    for path in paths[::stride][: args.count]:
        text = path.read_text(encoding="utf-8").rstrip("\n")
        head, _, _ = text.rpartition("\n\n")
        note = f"Revised in round {args.round}: the last paragraph here was out of date."
        path.write_text(f"{head}\n\n{note}\n", encoding="utf-8")
        print(path.relative_to(vault))
    print(f"sha256 {digest(vault)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    make = commands.add_parser("gen", help="write a new vault")
    make.add_argument("vault")
    make.add_argument("--notes", type=int, default=10000)
    make.add_argument("--scopes", type=int, default=5, choices=range(1, len(SCOPES) + 1))
    make.add_argument("--seed", type=int, default=1)
    make.set_defaults(run=gen)
    change = commands.add_parser("edit", help="change some notes of a vault")
    change.add_argument("vault")
    change.add_argument("--count", type=int, default=10)
    change.add_argument("--round", type=int, default=1)
    change.set_defaults(run=edit)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
