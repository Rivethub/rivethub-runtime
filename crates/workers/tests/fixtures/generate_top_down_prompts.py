"""Regenerate invented parity fixtures: python3 generate_top_down_prompts.py REF/scripts.

Imports the reference modules without running their CLIs. HTTP is replaced with
in-memory SSE, including a deterministic stand-in for the optional tokenizer.
"""

import argparse
import hashlib
import json
from pathlib import Path
import sys
import threading
from unittest.mock import patch
from uuid import UUID


def row(number, role, content, **tool):
    return {
        "id": str(UUID(int=number)),
        "role": role,
        "created_at": f"2026-05-14T13:{59 - number:02}:09Z",
        "content": content,
        "tool_name": tool.get("tool_name"),
        "tool_args": tool.get("tool_args"),
        "tool_result": tool.get("tool_result"),
    }


def event(body):
    return ("data: " + json.dumps(body) + "\n").encode()


class Playback:
    def __init__(self, runner, scripted):
        self.runner = runner
        self.scripted = scripted
        self.local = threading.local()
        self.requests = {}
        self.complete = runner.Session.complete

    def record(self, session, step, system, user, ceiling):
        self.local.step = step
        self.local.ceiling = ceiling
        return self.complete(session, step, system, user, ceiling)

    def http(self, request, *, timeout):
        assert request.full_url == "http://fixture.invalid/v1/chat/completions"
        assert timeout == 3600
        step = self.local.step
        self.requests[step] = {
            "step": step,
            "body": json.loads(request.data),
            "editorial_ceiling": self.local.ceiling,
        }
        choice, finish, usage = self.scripted(step)
        return Response(
            [event({"choices": [{"delta": {"content": choice}, "finish_reason": finish}]}),
             event({"usage": {"completion_tokens": usage}}), b"data: [DONE]\n"]
        )

    def patches(self):
        def complete(session, *args):
            return self.record(session, *args)

        return (
            patch.object(self.runner.Session, "complete", complete),
            patch.object(self.runner.urllib.request, "urlopen", self.http),
        )


class Response(list):
    def __enter__(self):
        return self

    def __exit__(self, *_):
        pass


def session(runner, prompts, model):
    return runner.Session(
        model, "invented", prompts, runner.PART_CEILING, runner.SESSION_CEILING,
        runner.CARRY_TRIGGER, runner.CARRY_CEILING, target_divisor=10, independent_parts=True,
    )


def fixture(runner, prompts, name, messages, chunk_tokens, window=262_144,
            calibrated=None, thinking=False, dense_exact=False):
    model = runner.Model("http://fixture.invalid/v1", "fixture-chat", window, chunk_tokens, thinking)
    blocks, tags = runner.render_messages(messages, *runner.clip_limits("full", None, None), True)
    rendered = "\n".join(text for _, text, _ in blocks)
    sample = rendered[:200_000]
    # Consult the reference calibration and exact-halving implementations with fake counts.
    if calibrated is not None:
        with patch.object(model, "exact", return_value=calibrated):
            model.measure(sample)

    def exact(text):
        return len(text) if dense_exact else model.tokens(text)

    chunk_rows = []
    consumed = {}
    spans_for = runner.spans_for

    def track(text, pieces, ranges):
        spans = spans_for(text, pieces, ranges)
        start = 0
        for piece in pieces:
            stop = start + len(piece)
            fragments = []
            for a, b, message_id in ranges:
                x, y = max(a, start), min(b, stop)
                if x < y:
                    offset = consumed.get(message_id, 0)
                    fragments.append({
                        "id": message_id,
                        "block_chars": [offset, offset + y - x],
                        "chunk_chars": [x - start, y - start],
                    })
                    consumed[message_id] = offset + y - x
            chunk_rows.append({"text": piece, "estimate": model.tokens(piece), "fragments": fragments})
            start = stop
        return spans

    with patch.object(model, "exact", exact), patch.object(runner, "spans_for", track):
        parts, spans = runner.chunks(blocks, model)
    transcript_tokens = sum(model.tokens(part) for part in parts)

    def canned(step):
        # Long enough to require several levels with the recursive fixture's meter.
        return ("Résultat du jardin" if step.startswith("part") else "Réduit.", "stop", 9)

    loop = Playback(runner, canned)
    complete, http = loop.patches()
    with complete, http:
        run = session(runner, prompts, model)
        result = run.run(parts, part_parallel=3)
    return {
        "name": name,
        "messages": messages,
        "calibration": calibrated,
        "ratio": model.chars_per_token,
        "dense_exact": dense_exact,
        "window": window,
        "chunk_tokens": chunk_tokens,
        "thinking": thinking,
        "rendered": rendered,
        "tags": tags,
        "render_keys": {
            message_id: hashlib.sha256(text.encode()).hexdigest()
            for _, text, message_id in runner.render_messages(messages, None, None, False)[0]
        },
        "chunks": chunk_rows,
        "spans": spans,
        "total_tokens": transcript_tokens,
        "requests": [loop.requests[call["step"]] for call in run.calls],
        "part_records": result["parts"],
    }


def sections(exporter, text, tags, owners):
    # Production projects the eval's individual block analysis into owning
    # ranges and offsets. It promotes nonempty preambles to Overview and
    # retains malformed-only blocks, unlike the eval recall-export policy.
    analysis = exporter.analyze(text, tags, {tags[tag] for tag in owners})
    by_id = {message_id: int(tag[1:]) for tag, message_id in tags.items()}
    fallback = [int(owners[0][1:]), int(owners[-1][1:])]
    rows = []

    def add(heading, start, body, ids):
        rows.append({
            "heading": heading,
            "chars": [start, start + len(body)],
            "bounds": [by_id[ids[0]], by_id[ids[-1]]] if ids else fallback,
            "verified": ids is not None,
        })

    if analysis.preamble.strip():
        add("Overview", 0, analysis.preamble, None)
    offset = len(analysis.preamble)
    for block in analysis.blocks:
        heading = block.text.split("\n", 1)[0]
        match = exporter.FORM.fullmatch(heading)
        title = heading[3:].rstrip()
        if match and match["title"].strip() and all(
            f"m{int(match[key][1:]):05d}" == match[key] for key in ("start", "end")
        ):
            title = match["title"]
        add(title, offset, block.text, block.message_ids)
        offset += len(block.text)
    return rows


def replies(runner, prompts, exporter):
    model = runner.Model("http://fixture.invalid/v1", "fixture-chat", 262_144, 90_000, False)
    tags = {f"m{n:05d}": row(n, "user", "")["id"] for n in range(1, 6)}
    cases = [
        ("covered and fallback", """\u001cBilan 🌼
## Jardin [m00002-m00003]
Premier.
## Sans plage
Deuxième.
## Cassée [m00002:m00003]
## Inversée [m00003-m00002]
## Hors partie [m00001-m00003]
## Jardin [m00002-m00003]
Même titre.
## Titre nu
 """, "stop", 300),
        ("no headings", "   Ça pousse.\r\n### Sous-titre   ", "stop", None),
        ("invalid only", "## Sans plage\nÉté.\n## Trous [m00003-m00005]", "stop", 1),
        ("bare valid heading", "## Graines [m00003-m00003]\r\n", "stop", 1),
        ("cut off", "## Graines [m00002-m00002]\nCommencé", "length", 300),
        ("over limit", "Détails.", "stop", 301),
        ("think and missing finish", "  <think>Calcul défait</think> Résultat. ", None, None),
        ("refusal", "  I'm sorry, no seedlings. ", "content_filter", 301),
        ("empty", " \r\n \u001f ", "length", 301),
    ]
    owners = ["m00002", "m00003", "m00005"]
    expected = []
    for name, text, finish, usage in cases:
        loop = Playback(runner, lambda _step: (text, finish, usage))
        call = session(runner, prompts, model)
        complete, http = loop.patches()
        with complete, http:
            try:
                output = call.complete("test", "unused", "unused", 300)
            except RuntimeError:
                output = None
        problems = call.calls[0]["problems"]
        flags = sum([
            1 if any(p.startswith("finish_reason ") for p in problems) else 0,
            2 if call.calls[0]["over_ceiling"] else 0,
            4 if "think tag in output" in problems else 0,
            8 if "refusal" in problems else 0,
        ])
        expected.append({
            "name": name, "text": text, "finish": finish, "usage": usage,
            "ceiling": 300, "output": output, "flags": flags,
            "sections": sections(exporter, output, tags, owners) if output else [],
        })
    return {"owners": [int(tag[1:]) for tag in owners], "cases": expected}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference_scripts", type=Path)
    args = parser.parse_args()
    sys.path.insert(0, str(args.reference_scripts.resolve()))
    import summarize as runner
    import export_sections as exporter

    prompts, prompts_path = runner.load_prompts("prompts_s1")
    small = fixture(runner, prompts, "short full tools", [
        row(1, "user", "Planter du basilic ?"),
        row(2, "assistant", ""),
        row(3, "tool", "Observation 🌱", tool_name="journal", tool_args={
            "z": "été\u0001\t\n行", "a": ["é", {"num": 1e-7, "scale": 1e20, "x": 1.0,
                                     "tie": 652282746268236.2, "small": -1.25e-5}],
            "description": "ligne longue\n" * 61,
        }, tool_result=("Heure\t🌿\n" * 43) + '{"récolte": true}\n'),
        row(4, "tool", "", tool_name="finish"),
        row(5, "assistant", "Oui.\nPrès d’une fenêtre."),
    ], 90_000, thinking=True)
    # Each user cut here would still fit under the full budget.
    user_turn = fixture(runner, prompts, "user turn and tiny tail", [
        row(1, "assistant", "a" * 218),
        row(2, "user", "é"),
        row(3, "assistant", "b" * 194),
        row(4, "user", "fin"),
    ], 100)
    assert user_turn["spans"] == [[str(UUID(int=n)) for n in group] for group in [[1], [2, 3], [4]]]
    large = fixture(runner, prompts, "oversize scalar split", [
        row(1, "assistant", "🌻" * 200),
        row(2, "user", "fin"),
    ], 38)
    assert len(large["chunks"]) >= 3
    dense = fixture(runner, prompts, "exact tokenizer halves", [
        row(1, "user", "à" * 900),
        row(2, "assistant", "é" * 800 + "\n" + "🌿" * 600),
    ], 1_000, dense_exact=True)
    assert len(dense["chunks"]) > 1

    recursive_messages = [row(n, "assistant", "texte 🌷 " * 9) for n in range(1, 9)]
    recursive = fixture(runner, prompts, "recursive composition", recursive_messages,
                        4_350, window=24_000, calibrated=30_000)
    assert len(recursive["part_records"]) == 8
    assert any(call["step"] == "final.group0.group0.group0" for call in recursive["requests"])
    result = {
        "runner_hash": runner.file_hash(Path(runner.__file__)),
        "prompts_hash": runner.file_hash(prompts_path),
        "storage_notes": [
            "Fixtures use ordered UUID rows and UTC timestamps. Product timestamps are normalized to UTC, while the runner slices the original stamp before any offset conversion.",
            "PostgreSQL JSONB determines key order before rendering. The fixtures preserve the input object's key order as the runner does after export.",
            "The product reader exposes object-valued arguments and may decode legacy JSON strings or lose unsupported numbers before rendering. The runner dumps the export value directly.",
            "The seed has no ModelRequest field. The product OpenAI adapter sets streaming and usage itself, so the test compares all three as profile metadata.",
            "Reply section rows use eval block/range analysis with the production Overview and fallback projection.",
        ],
        "sessions": [small, user_turn, large, dense, recursive],
        "replies": replies(runner, prompts, exporter),
    }
    path = Path(__file__).with_name("top_down_prompts.json")
    path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
