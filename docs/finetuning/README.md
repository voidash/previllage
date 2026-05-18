# Fine-Tuning And Eval Trail

This is the shortest path for a reviewer who wants to see how Gemma was
fine-tuned and why the final product uses Gemma behind resolver-first RAG
instead of as a naked factual chatbot.

## What To Read

1. [SFT v6 planner/composer](SFT_V6_PLANNER_COMPOSER.md) - the current
   direction and metrics.
2. [SFT v5 postmortem](SFT_V5_POSTMORTEM_AND_NEXT_PASS.md) - why the E4B v5
   adapter was not deployed even though training loss looked good.
3. [SFT v5 research](SFT_V5_RESEARCH.md) - research pass and data-design
   thinking for multi-turn service-navigation data.
4. [Benchmarks](BENCHMARKS.md) - older benchmark/eval notes, including CPT
   lessons.
5. [V4 handoff](V4_HANDOFF_TO_CODEX.md) and [SFT v4 prep](SFT_V4_PREP.md) -
   previous E2B/E4B iteration context.

## Training Scripts

The important scripts are:

- `scripts/build_sft_v6_4_split_task.py` - builds split planner,
  answerability, and final-composition rows.
- `scripts/format_sft_v6_planner_composer.py` - formats source-pack SFT
  records.
- `scripts/run_v6_4_g6e_qlora.sh` - AWS g6e training job for v6.4.
- `scripts/train_sft_v1.py` - shared QLoRA training harness.
- `scripts/eval_sft_v1.py` - quick eval runner for checkpoints.
- `scripts/eval_groundedness.py` - citation/groundedness checks.
- `scripts/sweep_v6_4_decode.py` - decode sweep for candidate checkpoints.
- `scripts/service_navigator_pipeline_audit.py`,
  `scripts/navigator_smoke_audit.py`, `scripts/rag_query_audit.py` - product
  smoke gates.

Reviewer-facing benchmark data lives in [`benchmarks/`](../../benchmarks/).
Small prompt seeds live in `seeds/`. Large SFT corpora are intentionally not
committed.

## Iteration Summary

| Pass | Result |
| --- | --- |
| CPT v1 | More Nepali text damaged chat behavior. CPT did not produce a service navigator. |
| SFT v2 | Roman-Nepali improved, but refusal/routing stayed brittle. |
| SFT v3a | More rows did not fix task design; general capability regressed. |
| SFT v5 E4B | Training looked good on loss but failed product smoke tests. Not deployed. |
| SFT v6.4 | First useful planner/composer candidate over provided source packs. |

Headline v6.4 smoke numbers from the source-backed `quick48` eval:

- URL recall: `0.94`
- Wrong refusals: `2/48`
- Source-ID citations: `45/48`
- Roman-Nepali degeneration/loops/mojibake: `0/10`

The deployed product should be judged as a pipeline: resolver, source router,
retrieval, source pack, Gemma composer, answer/citation UI. The model is not
expected to memorize government facts.

## Model Artifact Status

Current artifact names used during development:

- `voidash/gemma-helpdesk-v6-4-e4b-g6e-qlora-seed42`
- `voidash/gemma-helpdesk-v4b-e2b-seed42` with a staged GGUF build

These need a final public-release pass before being treated as submission
links: model card, eval card, license notes, and visibility flipped to public.
