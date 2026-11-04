#!/usr/bin/env python3
"""Mirrors the entity pages of a vault like ~/HQ into a folder kbx can index.

    mirror-hq.py ~/HQ ~/Library/Caches/kbx-bakeoff/hq

HQ's pages have `canonical_name` where kbx wants `title`, and lowercase types,
so `kbx index` can't read them. This reads them and writes new notes elsewhere,
keeping the id, so that labelling can happen in the copy and nothing, `kbx
accept` included, writes to the vault. It opens the vault for reading only.

It carries `id`, `type`, `canonical_name` and `aliases`, and the body. Every
other key stays behind, and so does every page outside TYPES, which leaves out
the financial ones. See docs/judge-bakeoff.md.
"""

import json
import os
import re
import sys

TYPES = {
    "company": "Company",
    "concept": "Concept",
    "person": "Person",
    "project": "Project",
    "tool": "Tool",
    "tool/technology": "Tool",
    "product": "Product",
}

SKIPPED = {"node_modules", "credentials", "repos", "dist", "build", "target"}


def scalar(frontmatter, key):
    found = re.search(rf"^{key}:[ \t]*(.+?)[ \t]*$", frontmatter, re.M)
    return found.group(1).strip("\"'") if found else None


def aliases(frontmatter):
    inline = re.search(r"^aliases:[ \t]*\[(.*)\][ \t]*$", frontmatter, re.M)
    if inline:
        words = inline.group(1).split(",")
    else:
        block = re.search(r"^aliases:[ \t]*\n((?:[ \t]*- .*\n?)+)", frontmatter, re.M)
        words = block.group(1).splitlines() if block else []
        words = [word.strip()[2:] for word in words]
    words = [word.strip().strip("\"'") for word in words]
    return [word for word in words if word]


def mirrored(text):
    """The note to write for a page's `text`, or None to leave it out."""
    if not text.startswith("---\n"):
        return None
    end = text.find("\n---", 4)
    if end < 0:
        return None
    frontmatter, body = text[4 : end + 1], text[end + 4 :].lstrip("-").lstrip("\n")
    identity = scalar(frontmatter, "id")
    kind = TYPES.get(scalar(frontmatter, "type") or "")
    title = scalar(frontmatter, "canonical_name")
    if not (identity and kind and title):
        return None
    keys = {"id": identity, "type": kind, "title": title, "aliases": aliases(frontmatter)}
    head = "".join(f"{key}: {json.dumps(value)}\n" for key, value in keys.items())
    return identity, f"---\n{head}---\n\n{body}"


def main(source, target):
    written = 0
    os.makedirs(os.path.join(target, "notes"), exist_ok=True)
    for folder, folders, files in os.walk(source):
        folders[:] = [f for f in folders if f not in SKIPPED and not f.startswith(".")]
        for name in files:
            if not name.endswith(".md"):
                continue
            try:
                with open(os.path.join(folder, name), errors="ignore") as file:
                    note = mirrored(file.read())
            except OSError:
                continue
            if note:
                identity, text = note
                with open(os.path.join(target, "notes", f"{identity}.md"), "w") as file:
                    file.write(text)
                written += 1
    print(f"{written} notes in {target}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    source, target = (os.path.abspath(os.path.expanduser(p)) for p in sys.argv[1:])
    if target == source or target.startswith(source + os.sep):
        sys.exit("the mirror has to be outside the vault")
    main(source, target)
