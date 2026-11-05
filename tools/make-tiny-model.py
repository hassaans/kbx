#!/usr/bin/env python3
"""Writes the tiny BERT model the tests load, the same bytes every time.

    tools/make-tiny-model.py tests/fixtures/tiny-model

Its weights are random, so its vectors mean nothing: it exists to run the real
model code without a download. The vocabulary is letters and digits, so any
English text tokenizes. Standard library only.
"""

import json
import random
import struct
import sys
from pathlib import Path

HIDDEN = 16
LAYERS = 1
HEADS = 2
INTERMEDIATE = 32
POSITIONS = 512

SPECIAL = ["[PAD]", "[UNK]", "[CLS]", "[SEP]"]
LETTERS = "abcdefghijklmnopqrstuvwxyz0123456789"
VOCAB = SPECIAL + list(LETTERS) + [f"##{letter}" for letter in LETTERS]


def tensors():
    rng = random.Random(1)

    def matrix(rows, columns):
        return [rows, columns], [rng.gauss(0, 0.2) for _ in range(rows * columns)]

    def vector(size, value=None):
        return [size], [rng.gauss(0, 0.02) if value is None else value for _ in range(size)]

    found = {}

    def linear(name, out, into):
        found[f"{name}.weight"] = matrix(out, into)
        found[f"{name}.bias"] = vector(out)

    def norm(name):
        found[f"{name}.weight"] = vector(HIDDEN, 1.0)
        found[f"{name}.bias"] = vector(HIDDEN, 0.0)

    found["embeddings.word_embeddings.weight"] = matrix(len(VOCAB), HIDDEN)
    found["embeddings.position_embeddings.weight"] = matrix(POSITIONS, HIDDEN)
    found["embeddings.token_type_embeddings.weight"] = matrix(2, HIDDEN)
    norm("embeddings.LayerNorm")
    for layer in range(LAYERS):
        base = f"encoder.layer.{layer}"
        for name in ("query", "key", "value"):
            linear(f"{base}.attention.self.{name}", HIDDEN, HIDDEN)
        linear(f"{base}.attention.output.dense", HIDDEN, HIDDEN)
        norm(f"{base}.attention.output.LayerNorm")
        linear(f"{base}.intermediate.dense", INTERMEDIATE, HIDDEN)
        linear(f"{base}.output.dense", HIDDEN, INTERMEDIATE)
        norm(f"{base}.output.LayerNorm")
    return found


def safetensors(found):
    header, blobs, at = {}, [], 0
    for name in sorted(found):
        shape, values = found[name]
        blob = struct.pack(f"<{len(values)}f", *values)
        header[name] = {"dtype": "F32", "shape": shape, "data_offsets": [at, at + len(blob)]}
        blobs.append(blob)
        at += len(blob)
    text = json.dumps(header, separators=(",", ":"), sort_keys=True).encode()
    text += b" " * (-len(text) % 8)
    return struct.pack("<Q", len(text)) + text + b"".join(blobs)


def config():
    return {
        "architectures": ["BertModel"],
        "attention_probs_dropout_prob": 0.0,
        "hidden_act": "gelu",
        "hidden_dropout_prob": 0.0,
        "hidden_size": HIDDEN,
        "initializer_range": 0.02,
        "intermediate_size": INTERMEDIATE,
        "layer_norm_eps": 1e-12,
        "max_position_embeddings": POSITIONS,
        "model_type": "bert",
        "num_attention_heads": HEADS,
        "num_hidden_layers": LAYERS,
        "pad_token_id": 0,
        "position_embedding_type": "absolute",
        "type_vocab_size": 2,
        "vocab_size": len(VOCAB),
    }


def tokenizer():
    ids = {token: index for index, token in enumerate(VOCAB)}
    return {
        "version": "1.0",
        "truncation": None,
        "padding": None,
        "added_tokens": [
            {
                "id": ids[token],
                "content": token,
                "single_word": False,
                "lstrip": False,
                "rstrip": False,
                "normalized": False,
                "special": True,
            }
            for token in SPECIAL
        ],
        "normalizer": {
            "type": "BertNormalizer",
            "clean_text": True,
            "handle_chinese_chars": True,
            "strip_accents": None,
            "lowercase": True,
        },
        "pre_tokenizer": {"type": "BertPreTokenizer"},
        "post_processor": {
            "type": "TemplateProcessing",
            "single": [
                {"SpecialToken": {"id": "[CLS]", "type_id": 0}},
                {"Sequence": {"id": "A", "type_id": 0}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 0}},
            ],
            "pair": [
                {"SpecialToken": {"id": "[CLS]", "type_id": 0}},
                {"Sequence": {"id": "A", "type_id": 0}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 0}},
                {"Sequence": {"id": "B", "type_id": 1}},
                {"SpecialToken": {"id": "[SEP]", "type_id": 1}},
            ],
            "special_tokens": {
                token: {"id": token, "ids": [ids[token]], "tokens": [token]}
                for token in ("[CLS]", "[SEP]")
            },
        },
        "decoder": {"type": "WordPiece", "prefix": "##", "cleanup": True},
        "model": {
            "type": "WordPiece",
            "unk_token": "[UNK]",
            "continuing_subword_prefix": "##",
            "max_input_chars_per_word": 100,
            "vocab": ids,
        },
    }


def main():
    folder = Path(sys.argv[1])
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "model.safetensors").write_bytes(safetensors(tensors()))
    (folder / "config.json").write_text(json.dumps(config(), indent=2) + "\n")
    (folder / "tokenizer.json").write_text(json.dumps(tokenizer(), indent=2) + "\n")


if __name__ == "__main__":
    main()
