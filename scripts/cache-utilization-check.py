#!/usr/bin/env python3
# Copyright 2025 itscheems
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Trace-derived cache-hit ratio regression gate.

Reads per-request jsonl trace files from a directory, extracts
``cache_read_input_tokens / prompt_tokens`` samples from ``token_usage``
events, and fails when the warm-turn (``step >= 2``) sample p50 drops
below the configured threshold.

Run with ``--self-test`` to exercise the pass / fail / missing-field
branches against synthetic fixtures (used by CI on every PR).

Run without ``--self-test`` to evaluate live traces from
``$ROKU_TRACES_DIR`` (defaults to ``~/.roku/traces``). Below the
``--min-samples`` floor the result is fail-soft (exit 0) so a fresh
clone with no captured traces does not flunk CI.

Threshold and observed baseline live in
``outputs/v0.0.13/token-economy/test-results-2/TC-21-openai.md``; the
default below is the canonical CI floor. CI never overrides the floor —
the script default is the single source of truth.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path
from statistics import median


DEFAULT_THRESHOLD = 0.50
DEFAULT_MIN_SAMPLES = 3
WARM_TURN_FLOOR = 2  # turn 1 is cold-start; exclude from samples


def collect_warm_turn_samples(
    trace_paths: list[Path], strict: bool = False
) -> tuple[list[float], int]:
    """Return warm-turn ``cache_read_input_tokens / prompt_tokens`` samples
    plus a count of skipped events whose ``token_usage`` payload is
    missing a required field (``model_id`` or ``cache_read_input_tokens``).

    Schema-drift handling: in ``strict`` mode (used by ``--self-test``)
    the missing-field branch raises ``ValueError`` so synthetic
    fixtures cannot regress the schema guard. In live-trace mode the
    same branch is a counted skip — older traces from before the
    field was wired in must not flunk a developer's local run, but
    the count surfaces in the verdict line so a real regression on
    new traces stays visible. Both ``model_id`` and
    ``cache_read_input_tokens`` are tracked through the same counter:
    silently fail-open skipping either field would let an adapter or
    runtime regression degrade the gate to ``warm_samples=0``
    SOFT_PASS without anyone noticing.

    Compaction summarizer events are excluded from the sample pool
    via the explicit ``is_compaction: true`` marker the runtime
    stamps on `LoopEvent::TokenUsage` when it fires inside reactive
    or mid-loop compaction. Those events carry a hard-coded
    ``cache_read_input_tokens=0`` (the summarizer's response cache
    info is not plumbed back) and would inject synthetic zero-ratio
    samples that drag the warm-turn p50 down even when normal
    decision calls are highly cached. The marker is preferred over
    inferring compaction from a missing ``model_id`` because the
    latter conflates a real schema-drift signal with an intentional
    filter.
    """
    samples: list[float] = []
    missing_field = 0
    for trace in trace_paths:
        with trace.open("r", encoding="utf-8") as handle:
            for line_num, raw in enumerate(handle, start=1):
                raw = raw.strip()
                if not raw:
                    continue
                try:
                    event = json.loads(raw)
                except json.JSONDecodeError as exc:
                    raise ValueError(
                        f"{trace}:{line_num} is not valid JSON: {exc}"
                    ) from exc
                if event.get("event") != "token_usage":
                    continue
                # Compaction summarizer events: skip silently. The
                # runtime stamps `is_compaction=true` exactly on
                # those, and they intentionally carry no real cache
                # info — they are not warm-turn samples.
                if event.get("is_compaction") is True:
                    continue
                # Real LLM-call events MUST carry `model_id` and
                # `cache_read_input_tokens`. Treat either missing as
                # schema drift: strict raises (fixtures cannot
                # regress), loose counts (legacy traces don't flunk
                # the gate) — but the count surfaces in the verdict
                # so a regression that nukes the field stays visible
                # instead of silently degrading to SOFT_PASS.
                if "model_id" not in event:
                    if strict:
                        raise ValueError(
                            f"{trace}:{line_num} token_usage event "
                            f"is missing `model_id` and is not marked "
                            f"`is_compaction=true`; refusing to silently "
                            f"drop (schema drift would hide a real "
                            f"regression by degrading the gate to "
                            f"warm_samples=0 SOFT_PASS)"
                        )
                    missing_field += 1
                    continue
                if "cache_read_input_tokens" not in event:
                    if strict:
                        raise ValueError(
                            f"{trace}:{line_num} token_usage event is "
                            f"missing `cache_read_input_tokens`; refusing "
                            f"to silently treat as 0 (schema drift would "
                            f"hide a real regression)"
                        )
                    missing_field += 1
                    continue
                step = event.get("step")
                prompt_tokens = event.get("prompt_tokens", 0)
                cache_read = event.get("cache_read_input_tokens", 0)
                if not isinstance(step, int) or step < WARM_TURN_FLOOR:
                    continue
                if not isinstance(prompt_tokens, int) or prompt_tokens <= 0:
                    continue
                if not isinstance(cache_read, int) or cache_read < 0:
                    continue
                samples.append(cache_read / prompt_tokens)
    return samples, missing_field


def evaluate(
    samples: list[float],
    threshold: float,
    min_samples: int,
    missing_field: int = 0,
) -> int:
    """Print a one-line summary plus PASS/FAIL/SOFT_PASS verdict and
    return the exit code: 0 on pass / soft-pass, 1 on regression.

    ``missing_field`` reports the count of legacy ``token_usage`` events
    that lacked ``cache_read_input_tokens``; surfaced in the verdict
    line so schema drift on new traces is visible without blocking
    pre-rollout traces from a developer's local run.
    """
    suffix = (
        f" missing_cache_field={missing_field}" if missing_field > 0 else ""
    )
    n = len(samples)
    if n == 0:
        print(
            f"warm_samples=0 threshold={threshold} verdict=SOFT_PASS "
            f"reason=no-samples{suffix}"
        )
        return 0
    p50 = median(samples)
    if n < min_samples:
        print(
            f"warm_samples={n} p50={p50:.4f} threshold={threshold} "
            f"verdict=SOFT_PASS reason=below-min-samples({min_samples}){suffix}"
        )
        return 0
    if p50 >= threshold:
        print(
            f"warm_samples={n} p50={p50:.4f} threshold={threshold} "
            f"verdict=PASS{suffix}"
        )
        return 0
    print(
        f"warm_samples={n} p50={p50:.4f} threshold={threshold} verdict=FAIL "
        f"reason=warm-turn-p50-below-floor{suffix}"
    )
    return 1


def run_check(traces_dir: Path, threshold: float, min_samples: int) -> int:
    if not traces_dir.is_dir():
        print(
            f"WARN: traces directory not found: {traces_dir}; treating as "
            f"no-samples (SOFT_PASS)",
            file=sys.stderr,
        )
        return evaluate([], threshold, min_samples)
    trace_paths = sorted(traces_dir.glob("loop-req-*.jsonl"))
    try:
        samples, missing_field = collect_warm_turn_samples(trace_paths)
    except ValueError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 2
    return evaluate(samples, threshold, min_samples, missing_field)


def run_self_test() -> int:
    """Validate three branches against synthetic fixtures so CI can
    exercise the gate without depending on captured live traces."""
    failures: list[str] = []
    with tempfile.TemporaryDirectory() as tmp_str:
        tmp = Path(tmp_str)

        # All real LLM-call fixtures carry `model_id` so the
        # compaction-event filter does not exclude them. Compaction
        # summarizer events emit with `model_id` absent — fixture E
        # exercises that exclusion path.
        model_id = "test-model"

        # Fixture A: warm samples all above the floor → PASS.
        good = tmp / "loop-req-good.jsonl"
        good.write_text(
            "\n".join(
                json.dumps(event)
                for event in [
                    {
                        "event": "token_usage",
                        "step": 1,
                        "prompt_tokens": 1000,
                        "cache_read_input_tokens": 0,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 2,
                        "prompt_tokens": 2000,
                        "cache_read_input_tokens": 1800,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 3,
                        "prompt_tokens": 2500,
                        "cache_read_input_tokens": 2100,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 4,
                        "prompt_tokens": 3000,
                        "cache_read_input_tokens": 2400,
                        "model_id": model_id,
                    },
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        good_samples, _ = collect_warm_turn_samples([good], strict=True)
        good_rc = evaluate(good_samples, DEFAULT_THRESHOLD, DEFAULT_MIN_SAMPLES)
        if good_rc != 0:
            failures.append(
                f"good-fixture: expected PASS (rc=0), got rc={good_rc}"
            )

        # Fixture B: all warm samples are 0 → FAIL.
        bad = tmp / "loop-req-bad.jsonl"
        bad.write_text(
            "\n".join(
                json.dumps(event)
                for event in [
                    {
                        "event": "token_usage",
                        "step": 2,
                        "prompt_tokens": 2000,
                        "cache_read_input_tokens": 0,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 3,
                        "prompt_tokens": 2500,
                        "cache_read_input_tokens": 0,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 4,
                        "prompt_tokens": 3000,
                        "cache_read_input_tokens": 0,
                        "model_id": model_id,
                    },
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        bad_samples, _ = collect_warm_turn_samples([bad], strict=True)
        bad_rc = evaluate(bad_samples, DEFAULT_THRESHOLD, DEFAULT_MIN_SAMPLES)
        if bad_rc != 1:
            failures.append(
                f"bad-fixture: expected FAIL (rc=1), got rc={bad_rc}"
            )

        # Fixture C: token_usage missing cache_read_input_tokens → loud raise.
        broken = tmp / "loop-req-broken.jsonl"
        broken.write_text(
            json.dumps(
                {
                    "event": "token_usage",
                    "step": 2,
                    "prompt_tokens": 2000,
                    "model_id": model_id,
                    # cache_read_input_tokens missing on purpose
                }
            )
            + "\n",
            encoding="utf-8",
        )
        try:
            collect_warm_turn_samples([broken], strict=True)
        except ValueError:
            pass
        else:
            failures.append(
                "broken-fixture: expected ValueError in strict mode on "
                "missing cache_read_input_tokens, got no exception"
            )
        # Loose mode (live-trace path) must NOT raise; the missing field
        # is reported via the counter so legacy traces don't flunk.
        loose_samples, missing = collect_warm_turn_samples(
            [broken], strict=False
        )
        if missing != 1 or loose_samples:
            failures.append(
                "broken-fixture: loose mode must skip the event and "
                "increment missing_field=1 with no samples; got "
                f"samples={loose_samples!r} missing={missing}"
            )

        # Fixture D: turn-1 (cold-start) samples must be ignored even when
        # they would otherwise pull p50 below the floor.
        cold = tmp / "loop-req-cold-start.jsonl"
        cold.write_text(
            "\n".join(
                json.dumps(event)
                for event in [
                    {
                        "event": "token_usage",
                        "step": 1,
                        "prompt_tokens": 5000,
                        "cache_read_input_tokens": 0,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 2,
                        "prompt_tokens": 5000,
                        "cache_read_input_tokens": 4500,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 3,
                        "prompt_tokens": 5500,
                        "cache_read_input_tokens": 4900,
                        "model_id": model_id,
                    },
                    {
                        "event": "token_usage",
                        "step": 4,
                        "prompt_tokens": 6000,
                        "cache_read_input_tokens": 5400,
                        "model_id": model_id,
                    },
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        cold_samples, _ = collect_warm_turn_samples([cold], strict=True)
        if any(value < 0.5 for value in cold_samples):
            failures.append(
                "cold-start-fixture: turn-1 sample leaked into warm pool"
            )

        # Fixture E: compaction summarizer events (marked
        # `is_compaction=true`) must be excluded from the sample pool.
        # Without the filter, each compaction event injects a synthetic
        # 0.0 ratio sample; with three compaction events alongside
        # three healthy 0.6-ratio LLM-call events the cumulative median
        # is 0.0 (FAIL) even though warm-turn cache utilization is at
        # the threshold. The filter restores the median to 0.6 (PASS).
        compact = tmp / "loop-req-compaction.jsonl"
        compact_events = []
        for step in (2, 3, 4):
            compact_events.append(
                {
                    "event": "token_usage",
                    "step": step,
                    "prompt_tokens": 1000,
                    "cache_read_input_tokens": 600,
                    "model_id": model_id,
                }
            )
            compact_events.append(
                {
                    "event": "token_usage",
                    "step": step,
                    "prompt_tokens": 200,
                    "cache_read_input_tokens": 0,
                    # `is_compaction=true` is the explicit positive
                    # marker the runtime stamps on compaction
                    # summarizer events. Note: `model_id` is also
                    # absent here (the summarizer doesn't surface its
                    # serving model), but the filter does NOT key off
                    # that — `is_compaction` is the canonical signal
                    # so a future regression that drops `model_id`
                    # from real LLM calls cannot impersonate a
                    # compaction event.
                    "is_compaction": True,
                }
            )
        compact.write_text(
            "\n".join(json.dumps(event) for event in compact_events) + "\n",
            encoding="utf-8",
        )
        compact_samples, _ = collect_warm_turn_samples([compact], strict=True)
        if len(compact_samples) != 3 or any(
            abs(value - 0.6) > 1e-9 for value in compact_samples
        ):
            failures.append(
                "compaction-fixture: compaction summarizer events must be "
                "excluded; expected exactly 3 samples of 0.6 (one per "
                f"warm LLM-call step), got {compact_samples!r}"
            )
        compact_rc = evaluate(
            compact_samples, DEFAULT_THRESHOLD, DEFAULT_MIN_SAMPLES
        )
        if compact_rc != 0:
            failures.append(
                "compaction-fixture: filtered samples should PASS at "
                f"threshold {DEFAULT_THRESHOLD}; got rc={compact_rc}"
            )

        # Fixture F: a real LLM-call event missing `model_id` (and not
        # marked `is_compaction=true`) is schema drift, not a
        # compaction event. Strict mode must raise; loose mode must
        # count it through `missing_field` and exclude it from the
        # sample pool — the regression scenario this guards against
        # is an adapter / runtime change that silently nukes
        # `model_id` from real LLM-call events, which under a
        # fail-open skip would degrade the gate to warm_samples=0
        # SOFT_PASS and bypass the regression check entirely.
        drift = tmp / "loop-req-model-id-drift.jsonl"
        drift.write_text(
            json.dumps(
                {
                    "event": "token_usage",
                    "step": 2,
                    "prompt_tokens": 2000,
                    "cache_read_input_tokens": 1800,
                    # `model_id` deliberately omitted, no
                    # `is_compaction` marker — this is the schema-drift
                    # case the gate must surface.
                }
            )
            + "\n",
            encoding="utf-8",
        )
        try:
            collect_warm_turn_samples([drift], strict=True)
        except ValueError:
            pass
        else:
            failures.append(
                "model-id-drift-fixture: expected ValueError in strict "
                "mode on missing `model_id` without `is_compaction` "
                "marker, got no exception"
            )
        drift_samples, drift_missing = collect_warm_turn_samples(
            [drift], strict=False
        )
        if drift_missing != 1 or drift_samples:
            failures.append(
                "model-id-drift-fixture: loose mode must skip the "
                "event and increment missing_field=1 with no samples; "
                f"got samples={drift_samples!r} missing={drift_missing}"
            )

    if failures:
        print("SELF-TEST FAILURES:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("self-test: PASS")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Trace-derived cache-hit ratio regression gate."
    )
    parser.add_argument(
        "traces_dir",
        nargs="?",
        default=None,
        help=(
            "Directory containing loop-req-*.jsonl traces. Defaults to "
            "$ROKU_TRACES_DIR or ~/.roku/traces."
        ),
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=DEFAULT_THRESHOLD,
        help=(
            f"Warm-turn p50 floor. Default {DEFAULT_THRESHOLD}; canonical "
            f"value lives in this script (single source of truth)."
        ),
    )
    parser.add_argument(
        "--min-samples",
        type=int,
        default=DEFAULT_MIN_SAMPLES,
        help=(
            f"Below this many warm samples the run is fail-soft (rc=0). "
            f"Default {DEFAULT_MIN_SAMPLES}."
        ),
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run synthetic-fixture self-tests instead of evaluating live traces.",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return run_self_test()

    traces_dir = (
        Path(args.traces_dir)
        if args.traces_dir
        else Path(
            os.environ.get(
                "ROKU_TRACES_DIR",
                str(Path.home() / ".roku" / "traces"),
            )
        )
    )
    return run_check(traces_dir, args.threshold, args.min_samples)


if __name__ == "__main__":
    sys.exit(main())
