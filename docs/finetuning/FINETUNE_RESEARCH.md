# FINETUNE_RESEARCH.md — research log for fine-tuning Gemma for Nepali

Living research log. Goal: a defensible SFT/CPT recipe that does **not** repeat
CPT v1 (BENCHMARKS.md §3.2 — chat collapse from ~9.5% instruction-format data).

This file is iterative. Each pass reads a batch of papers, documents them
under "Per-paper notes", then updates "Cross-cutting synthesis" and the
"Concrete plan" at the bottom. Stop condition: synthesis answers
(a) data mix, (b) anti-forgetting technique, (c) PEFT vs full FT, (d) eval
target — with citations to specific papers.

**Pass log**:
- Pass 1 (2026-04-28): 5 papers — STM (2501.14315), GemMaroc (2505.17082), Marathi-LoRA (2411.18571), GaMS-3 (2603.01691), Romanized-Nepali (2604.14171).
- Pass 2 (2026-04-28): 4 papers + rsLoRA — Gemma 3 tech report (2503.19786), Gemma 2 tech report (2408.00118), Borsch / Ukrainian Gemma (2404.09138), Tsinghua low-resource project (2510.04139), rsLoRA (2312.03732).
- Pass 3 (2026-04-28): **Confirmed actual base = Gemma 4 E4B-IT** (released 2026-04-02). 4 substantive reads + 3 abstracts — Gemma 4 HF blog (architecture, PLE, MatFormer, Apache 2), Navarasa 2.0 (Indic Gemma 1 incl. Nepali), IndicLLMSuite (2403.06350; 12.9B Nepali pretraining tokens), Indic Capabilities (2501.13912; Nepali excluded from core ranking), QLoRA abstract (2305.14314), DoRA abstract (2402.09353), Secret Recipe abstract (2412.13337).
- Pass 3 supplement (2026-04-28): **Discovered Gemma 4 chat template is BREAKING-CHANGE from Gemma 3** — uses `<|turn>...<turn|>`, not `<start_of_turn>...<end_of_turn>`. Read HF + Google AI model cards, found `chat_template.jinja` file, identified operational loading gotcha (HF transformers issue #45205). Found **MedInjection-FR (2603.06905)** — first controlled native-vs-MT-vs-synthetic experiment for instruction tuning.
- Pass 4 (2026-04-28): 3 papers + 2 operational — MedInjection-FR (controlled NAT/TRAD/SYN data ablation), MURI (2409.12958, reverse-instruction pipeline), Lebanese (2505.00114, native-quality > scale; CPO underperforms SFT), Saugatkafley dataset audit (confirmed MT-Alpaca, no native content), vLLM Gemma 4 deployment recipe.
- Pass 5 (2026-04-28): 3 final reads (consolidated) — IndicGenBench (2404.16816) confirms Nepali included in 29-language benchmark; LoRA original (2106.09685) historical context; Secret Recipe (2412.13337) full-FT-only hyperparam guide. None change v0.4 architecture; research at natural stopping point.

---

## Why this exists

- **CPT v1 regressed** (BENCHMARKS.md §3.2). Mechanism known: data was almost
  entirely raw Nepali text; instruction-format data was ~9.5%; chat behavior
  was overwritten.
- **No SFT data exists yet.** Plan was 5–10k distilled tuples from
  Sonnet/Kimi K2.6, not yet generated.
- **No groundedness eval exists** (task #32, prerequisite per CLAUDE.md).
- We need to read the literature before training again, not after.

---

## Reading queue

Priority = how directly the paper answers a CPT-v1-style failure or matches
our setup (Gemma family, low-resource Indic-adjacent, RAG composer use case).

### Tier 0 — actual base model (Gemma 4)

- [x] **Gemma 4 HF blog** (huggingface.co/blog/gemma4) — architecture, PLE, MatFormer, Apache 2
- [x] **`google/gemma-4-E4B-it` model card on HuggingFace** — chat template confirmed `<|turn>...<turn|>`, MMMLU 76.6%
- [x] **Gemma 4 official Google AI for Developers model card** — variants table, architecture details, no Indic benchmarks
- [x] **`chat_template.jinja` file** + HF transformers issue #45205 — operational loading gotcha
- [ ] vLLM Gemma 4 recipe (github.com/vllm-project/recipes/blob/main/Google/Gemma4.md) — operational deployment guide
- [ ] Gemma 4 thinking-mode docs (ai.google.dev/gemma/docs/capabilities/thinking) — decide on/off for SFT

### Indic-specific work (added Pass 3)

- [x] **Navarasa 2.0** (Telugu LLM Labs, 2024) — Gemma 1 Indic incl. Nepali (light docs)
- [x] **2403.06350** — IndicLLMSuite / AI4Bharat (Sangraha + IndicAlign-Instruct)
- [x] **2501.13912** — Analysis of Indic Language Capabilities in LLMs

### Tier 1 — directly maps to CPT v1 failure mode or our exact setup

- [x] **2501.14315** — Mitigating Forgetting via Low-Perplexity Token Learning (STM)
- [x] **2505.17082** — GemMaroc: Darija with minimal data
- [x] **2411.18571** — Challenges Adapting Multilingual LLMs via LoRA PEFT (Marathi)
- [x] **2604.14171** — Romanized Nepali benchmarking (Llama/Mistral/Qwen) ⭐ direct
- [x] **2603.01691** — GaMS-3: Slovene Gemma 3 12B (full-FT CPT + IT)

### Tier 2 — closest existing recipes

- [x] **2503.19786** — Gemma 3 Technical Report (Pass 2)
- [x] **2408.00118** — Gemma 2 Technical Report (Pass 2)
- [x] **2404.09138** — From Bytes to Borsch / Ukrainian Gemma (Pass 2) ⭐ replicates CPT v1 failure
- [ ] **2403.08295** — Gemma 1 original (likely skip — superseded by Gemma 2/3 reports)

### Tier 3 — methods primer

- [x] **2312.03732** — rsLoRA (Kalajdzievski 2023) — verified, used in our recipe
- [x] **2510.04139** — Tsinghua Final Report (course project; key finding: SFT+RAG > SFT alone)
- [ ] **2305.14314** — QLoRA (Dettmers et al) — high priority since we use 4-bit NF4
- [ ] **2106.09685** — LoRA (Hu et al)
- [ ] **2412.13337** — Unveiling the Secret Recipe: SFT Small LLM hyperparams
- [ ] **2504.06225** — Encoder-Decoder Gemma (peripheral)
- [ ] **2408.00690** — Contrastive embedding tuning (relevant to retrieval, not composer)
- [ ] **2406.13626** — Gemma-7B sentiment SFT (peripheral)
- [ ] **2512.00219** — Minimal-Edit Instruction Tuning for Low-Resource Indic GEC (peripheral)

### To find — not yet on the list

- [ ] Indic-Gemma / IndicLLM / IndicGemma adaptation papers (target Nepali specifically)
- [ ] Devanagari tokenizer/vocabulary extension (SentencePiece extension)
- [ ] Sailor / SEA-LION recipe (analogous low-resource family adaptation)
- [ ] Catastrophic-forgetting-in-CPT survey
- [ ] Replay/rehearsal mixing ratios for LM continued pretraining
- [ ] DoRA, PiSSA, rsLoRA original papers (rsLoRA used in Romanized Nepali paper, not yet read)
- [ ] Native-vs-MT controlled comparison (anyone published this for Indic?)

---

## Per-paper note template

```
### <arxiv-id> — <short title>
- Citation: full title, authors, year, venue
- Setup: base model, target language(s), data size, compute
- Method: technique + key hyperparams (LR, rank, batch, steps, mix)
- Results: numbers — what improved, what regressed
- Takeaway for us: 1–3 bullets on how this changes our plan
- Confidence: read in full / skimmed sections / abstract only
```

---

## Per-paper notes (newest first)

### Pass 5 consolidated — IndicGenBench, LoRA original, Secret Recipe

These three tighten margins; none change v0.4's architecture.

**IndicGenBench (2404.16816, Google Research)** — Multi-way parallel benchmark, **29 Indic languages including Nepali**, 13 scripts, 4 language families. Four generation tasks: cross-lingual summarization, MT, cross-lingual QA, NER. Human-curated. Repo: `google-research-datasets/indic-gen-bench`. The Indic Capabilities paper (2501.13912) reports Gemma 3 27B at 63.4% overall on IndicGenBench; per-language Nepali numbers are inside the actual benchmark scores (would need to fetch). **Action: add IndicGenBench-Nepali to v0.4 eval plan as our open-domain Nepali generation slot.**

**LoRA original (2106.09685, Hu et al., 2021)** — historical context. Original recipe: **r=1-8 on `{Wq, Wv}` only**; rank diminishes returns very fast (r=1: 73.4 vs r=64: 73.5 on GPT-3 WikiSQL). MLP "frozen for simplicity and parameter-efficiency." Used LR 2e-4, batch 128 on GPT-3 175B. **Modern recipe (r=32 rsLoRA, all attention+MLP modules) departs deliberately**: harder tasks (low-resource language adaptation, not GLUE); rsLoRA fixes gradient norm collapse at high r; downstream evidence from GemMaroc / Romanized Nepali / Lebanese supports broader target_modules. **No change to v0.4.** Note: the original paper explicitly does NOT discuss instruction tuning or applying LoRA to already-instruction-tuned checkpoints — that's modern usage, not the original framing.

**Secret Recipe (2412.13337, Pareja et al., Dec 2024)** — Tested Granite 3B/7B, Llama 3.2 3B, Mistral 7B. **Full FT only** (paper: "we did not investigate parameter-efficient fine-tuning strategies, such as LoRA"). Sweep: LR 1e-6 to 1e-4; batch 128/3840/7680; warmup 0/25/100; epochs 3-10.

Optimal full-FT config:

| Hyperparam | Recommended            |
|-----------|-------------------------|
| LR        | 2e-5 (Granite) / 1e-6 (Mistral) |
| Batch     | 3840-7680 samples       |
| Epochs    | 10                      |
| Warmup    | **0 steps**             |
| Schedule  | **Constant** (no decay) |

Refutations: TULU's batch 128 → MMLU 0.48 vs their batch 4K → 0.50 (Table 5). Cosine decay vs none → 0.525 vs 0.524 (negligible). 100-step warmup matches 0-step (Goyal 2017 challenged). **Stacked training beats phased**: 3.7M samples for stacked vs 7.9M for phased to reach MMLU 0.53 vs 0.52.

Early-stop heuristic: lower gradient norms + higher training loss in early epochs → better final perf. Use as a kill signal for bad runs.

**Caveat for v0.4**: their LRs are full-FT scale (~10× lower than LoRA LRs). The principle "larger batch + lower LR" generalizes; absolute numbers do not. Possible v0.4 ablation: batch 32 + LR 5e-5 (per Secret Recipe direction) vs current batch 16 + LR 1e-4 (per Romanized Nepali / GemMaroc). **No change to v0.4 defaults.**

**Confidence**: IndicGenBench from search summary (paper not fully read); LoRA + Secret Recipe HTMLs read in detail.

---

### 2603.06905 — MedInjection-FR: Native vs MT vs Synthetic Controlled Experiment ⭐⭐ (the experiment we were waiting for)

**Citation**: Belmadani, El Khettari, Constant dit Beaufils, Favre, Dufour. arxiv:2603.06905v1, March 2026. Aix-Marseille / Nantes / CHU Nantes / Grenoble Alpes.

**Setup**:
- French biomedical QA. **Qwen-4B-Instruct** as base (size analog to our Gemma 4 E4B).
- 571,436 total instruction pairs; 7 controlled setups all normalized to **33,493 examples**:

| Slice           | Total available | Source                                                      |
|-----------------|-----------------|-------------------------------------------------------------|
| Native (NAT)    | 77,247          | FrenchMedMCQA, MediQAl, FrBMedQA, S-Editions, medical-cases-fr |
| Synthetic (SYN) | 76,506          | DEFT-2021, DIAMED, MORFITT prompts → GPT-4o generated         |
| Translated (TRAD)| 417,674        | EN MedQA, PubMedQA, MedMCQA, MMLU-medical → Gemini 2.0 Flash + GPT-4o-mini |

**Method**: **DoRA r=16, α=16, dropout 0.05**, target_modules=`q,k,v,o,gate,up,down` (same as our recipe). 10 epochs, batch 12, LR 1e-4, gradient accumulation 8, cosine 0.05 warmup. Greedy decoding, three randomized runs.

**Headline results** (aggregated EM, MCQ + MCQU):

| Config       | Aggregated EM |
|--------------|---------------|
| Qwen-4B base | 46.74         |
| **NAT only** | 47.13         |
| TRAD only    | 45.66         |
| **SYN only** | **38.23** ⚠️  |
| **NAT+TRAD** | **49.24** ✅ best |
| NAT+SYN      | 46.72         |
| TRAD+SYN     | 45.09         |
| ALL          | 48.46         |

**Key insight (quote)**: "Despite containing only half or one-third of the native examples, mixed configurations such as NAT-TRAD, NAT-SYN, and ALL achieve comparable or even slightly higher aggregated scores."

NAT+TRAD with **half the native data** outperforms NAT alone.

**Other findings**:
- BLEU / ROUGE / BERTScore correlate weakly with human judgment (r=0.02–0.36).
- **LLM-as-judge** (MedGemma-27B) correlates well with experts (r=0.61, p<0.001).
- Position bias: randomizing MCQ answer order changes absolute scores (e.g., 37.31 → 23.20) but **not relative rankings**.

**Takeaway for us** (load-bearing):
1. **Native data is the anchor; mixtures beat singles.** Our v0.3.1 plan (70% distilled + 20% EN + 10% Roman-NE) lacks a native Nepali anchor. **Add 10-25% native slice.** Even small (~2K) native dose changes outcomes.
2. **Synthetic-only collapses (−9 EM)** at this scale. Distilled tuples from Sonnet/Kimi are essentially synthetic. **Without native and translated mix-in, our v0.3.1 risks the same collapse.**
3. **DoRA (r=16) was their choice over LoRA.** Same target_modules we plan. Worth ablating: rsLoRA r=32 vs DoRA r=16 on Gemma 4 E4B.
4. **BLEU is unreliable** for our task. LLM-as-judge correlates better — can use Sonnet/Kimi as judges in the groundedness eval.
5. **Position-bias check**: when running Belebele/MMLU multiple-choice evals, randomize answer ordering across runs. Otherwise we're measuring memorization of A/B/C/D layout.

**Confidence**: Read in detail (HTML), full tables extracted.

---

### 2409.12958 — MURI: Reverse Instructions for Low-Resource Languages

**Citation**: Köksal, Thaler, Imani, Üstün, Korhonen, Schütze. 2024. arxiv:2409.12958.

**Method**: Reverse instruction generation. Pipeline:
1. Pick high-quality native-language document `d_τ` (e.g., from Wikipedia, CulturaX).
2. Translate to English `d_ϵ` via MADLAD-400-3B-MT.
3. Prompt LLM (Mixtral-8x7B): "What instruction could this answer satisfy?" → English instruction `i_ϵ`.
4. Back-translate `i_ϵ` to target language → `i_τ`.
5. Final pair: `(i_τ, d_τ)` — **output stays in original native language**, only instruction round-trips through English.

This dodges "translationese" in the *output* (which is what models learn to emulate).

**Scale**: 200 languages, **2.2M instruction pairs**, 64% from Joshi-low-resource. Sources: Wikipedia (1M), CulturaX (687K), WikiHow (55K), NLP tasks (455K).

**Nepali coverage**: Mentioned in references but **not in the final dataset.** Hindi/Bengali/Gujarati/Kannada/Malayalam/Tamil/Telugu represented.

**Recipe (mT5-XXL SFT)**:
- Input/output 1024 tokens
- Effective batch 64 (8× grad accum)
- LR **3e-4 fixed, no scheduler**
- 5 epochs
- Nucleus sampling (top_p=0.8, T=0.9) at inference

**Results**:
- Multilingual MMLU 31 langs: MURI 36.0% > mT0 31.5% (+14% relative).
- TranslatedDolly 21 langs: MURI **59% win rate** vs mT0 28%.
- Aya alone 35.1% → Aya + MURI 37.2% (Taxi1500, low-resource monolingual).

**Native-speaker quality scores** (13 langs): alignment 4.01/5, instruction correctness 4.65/5, output correctness 4.41/5.

**Failure modes**: Languages without orthographic standardization (Bavarian, Mandarin/Shanghai dialect mix) suffered. Code-switching observed.

**Implementation**: github.com/akoksal/muri. MURI-IT dataset on HF.

**Takeaway for us**:
1. **The reverse-instruction pipeline is reproducible for Nepali.** Use Sangraha Verified (1.8B native tokens) → MADLAD-400 → Mixtral-8x7B (or Sonnet) for instruction generation → MADLAD-400 back to Nepali. Cost is modest if we cap to ~10K examples.
2. **Output stays native**: solves the translationese-output problem that hits Borsch / Marathi / Saugatkafley.
3. **Worth as our 10-15% native slice** — complements distilled task tuples + Anudesh subset.
4. **MURI does not include Nepali in its 200 published languages** — gap we can fill.

**Confidence**: Read in detail (HTML).

---

### 2505.00114 — Lebanese Low-Resource Dialect Translation

**Citation**: Yakhni & Chehab, 2025. American University of Beirut. arxiv:2505.00114v1.

**Setup**:
- Base: **Aya23-8B** (Cohere multilingual). No Gemma. Resource constraints.
- Lebanese (low-resource Arabic dialect — analogous to Roman-NE).
- Data:
  - **Language Wave (LW)**: ~3K native sentences from podcasts.
  - **OpenSubtitles (OS)**: 128K translated sentences (movie subtitles).
  - **MADAR**: 12K translated.
  - **LGID Grammar**: 2,836 synthetic from grammar book via Claude 3.5 Sonnet.

**Method**: QLoRA, **r=64**, batch 16, gradient accum 16, 3 epochs, 4×L40S. LR not disclosed.

**Results** (xCOMET-10.7B reference-free):

| Test set                | Best config                  | Score | vs Vanilla |
|-------------------------|------------------------------|-------|------------|
| **LebEval** (native)    | Instruct-Cont-LW + C3-shot   | 74.4  | 68.7 (+5.7)|
| **FLoRes** (translated) | Instruct-Cont-NN + C3-shot   | 89.1  | 85.5 (+3.6)|

**Quote**: "Fine-tuning on culturally aware datasets yields superior results... emphasizing the critical role of data quality over quantity."

**3K native data > 140K translated data on the native test set**. On a translated test set the gap closes — confirming the eval-set choice changes the conclusion.

**Other findings**:
- **Curriculum learning failed** (Cont+MT, Grammar+Cont+MT) — catastrophic forgetting.
- **CPO (Contrastive Preference Optimization) consistently underperformed SFT** — "often yielding results below the baseline model's performance." Direct evidence that preference methods don't help low-resource at this scale.
- **Random K=3 few-shot** matches embedding-based selection; simpler is fine.
- Synthetic grammar instructions underperformed (LGID).

**Takeaway for us**:
1. **Quality > quantity confirmed for the third time** (after MedInjection-FR and GemMaroc). 3K native > 140K MT for the actual eval.
2. **Skip preference learning (DPO/CPO) for v0.4.** Lebanese paper shows it actively hurts at low-resource scale. Save it for v1.0+ if/when we have human preference data.
3. **Use a native eval set** (LebEval-equivalent for Nepali = task #32 groundedness). Don't trust translated test sets to measure dialect/low-resource quality.
4. **Curriculum learning is risky.** Single-stage SFT on a balanced mix is safer than phased training.
5. **r=64 worked for them** at 8B scale; Romanized Nepali used r=32 at 7-8B; we plan r=32. Reasonable; could ablate r=64 if v0.4 underperforms.
6. **xCOMET-equivalent for Nepali** — does it exist? Worth searching. Otherwise use Sonnet/Kimi as LLM-as-judge per MedInjection-FR.

**Confidence**: Read in detail (HTML), tables extracted.

---

### Saugatkafley/alpaca-nepali-sft — dataset audit (Pass 4)

**Source**: huggingface.co/datasets/Saugatkafley/alpaca-nepali-sft. Card visible at the dataset page.

**Findings**:
- **52,005 examples**, single train split (no val/test).
- **Devanagari only** — no Roman-Nepali present.
- Schema: `instruction` (9-476 chars) / `input` (0-2.3K) / `output` (0-4.12K) / `id`.
- **Translated from Alpaca** (naming convention; no methodology documented; no quality filter; no license).
- Sample 1: `"स्वस्थ रहन तीनवटा टिप्स दिनुहोस्"` (Give three tips to stay healthy) → generic health tips. **Direct MT of Alpaca's English original.**
- Sample 2: `"संसारको सबैभन्दा प्रसिद्ध चित्रकार को हुन?"` (Who is the world's most famous painter?) → Leonardo da Vinci. **No Nepal-domain content.**

**Downstream models trained on it** (per HF page): `shivam9980/NEPALI-LLM-INSTRUCT` (9B), GGUF variants, `vhab10/Llama-3.2-3B-Instruct_Nepali_4bit`.

**Verdict**:
- **NOT native Nepali authoring.** It's MT Alpaca through-and-through.
- **No Nepal-domain knowledge.** Cannot serve as groundedness training data.
- **Useful as a language-fluency slice only** — Nepali Devanagari surface form. Comparable to UAlpaca in the Borsch paper, which we know causes generation collapse if it dominates.
- **Demote to lowest-priority data candidate.** If used at all, cap at ≤10% of mix.

**Takeaway**: Saugatkafley is not a substitute for native Nepali instruction data. The IndicAlign-Instruct Nepali subset (especially Anudesh crowdsourced) and a MURI-style native pipeline are higher-quality alternatives.

**Confidence**: Direct dataset inspection on HF.

---

### Gemma 4 chat template, model cards, operational details (Pass 3 supplement)

**Sources**:
- HF model card: `huggingface.co/google/gemma-4-E4B-it`
- Google AI for Developers card: `ai.google.dev/gemma/docs/core/model_card_4`
- Chat template file: `huggingface.co/google/gemma-4-E4B-it/blob/main/chat_template.jinja`
- HF transformers issue #45205 (chat template loading gotcha)

**The actual Gemma 4 chat template** (BREAKING CHANGE from Gemma 2/3):

Gemma 4 uses **`<|turn>` and `<turn|>` markers — NOT `<start_of_turn>` / `<end_of_turn>`**.

Approximate format:
```
<bos><|turn>system
[system instructions, optional <|think|> to enable thinking]
<turn|>
<|turn>user
[user prompt]
<turn|>
<|turn>model
[<|channel>thought\n[reasoning]<channel|>] (only if thinking enabled)
[answer]
<turn|>
```

(Exact whitespace/newline behavior is in the jinja file; framework will handle.)

**Thinking mode**: `<|think|>` in system prompt → output wraps internal reasoning in `<|channel>thought\n...<channel|>`. Can be disabled: `apply_chat_template(..., enable_thinking=False)`.

**Native system role**: Gemma 4 has explicit `system` role support (Gemma 3 did not). This is the **right channel for our RAG context injection** (system = retrieved chunks; user = question; model = grounded answer).

**Operational gotcha**: Chat template ships as a separate `chat_template.jinja` file, **NOT** embedded in `tokenizer_config.json`. `tokenizer.apply_chat_template()` raises `ValueError: tokenizer.chat_template is not set` out-of-the-box.

Workaround:
```python
from huggingface_hub import hf_hub_download
template_path = hf_hub_download("google/gemma-4-E4B-it", "chat_template.jinja")
with open(template_path) as f:
    tokenizer.chat_template = f.read()
```

This is a known issue (HF transformers #45205); will likely be fixed in a future release. Either way, plumb the manual load into our data prep.

**Official architecture details (from Google AI model card)**:

| Property      | E2B  | E4B  | 26B-A4B     | 31B   |
|---------------|------|------|-------------|-------|
| Effective     | 2.3B | 4.5B | 3.8B active | 30.7B |
| With embeds   | 5.1B | 8B   | 25.2B (MoE) | 30.7B |
| Context       | 128K | 128K | 256K        | 256K  |
| Sliding window| 512  | 512  | 1024        | 1024  |
| Layers        | -    | 42   | -           | -     |
| Modalities    | T+I+A| T+I+A| T+I         | T+I   |

- **Per-Layer Embeddings (PLE)**: confirmed in official card. Each decoder layer has its own small embedding table.
- **Unified Keys and Values in global layers**: confirmed (= "Shared KV Cache" in HF blog).
- **Proportional RoPE (p-RoPE)**: new positional encoding for long context.
- **MatFormer**: NOT in official Google card. Only in HF blog. May be a description of the design philosophy more than a hard architectural commitment.
- **Native system role** confirmed.
- E4B has audio encoder (~300M); 26B/31B do not.
- No MGSM, IndicGenBench, FLORES-Nepali numbers in either model card.
- **MMMLU 76.6% on E4B-IT** — strong multilingual baseline.
- Data cutoff: January 2025.
- License: **Apache 2.0** (confirmed).

**Takeaway for us**:
1. **Recipe v0.3's chat template was wrong.** Update to v0.3.1 with `<|turn>...<turn|>` format. The Gemma 3 → Gemma 4 break is the kind of one-line bug that silently destroys SFT results — exactly the failure mode CPT v1 hit. Do not skip this.
2. **Use `system` role for RAG context injection.** Cleanest channel for our retrieved chunks — separates retrieval from question, lets the loss focus on the model turn.
3. **Disable thinking mode for v0.3.1.** We're fine-tuning a grounded composer, not a reasoner. Our SFT data should not contain `<|channel>thought\n...<channel|>` wrappers. If we want a reasoner later, that's a separate experiment.
4. **Plumb manual chat-template loading into data prep.** Without it, every formatted example will be silently wrong.
5. **MMMLU 76.6% is a strong baseline** — multilingual reasoning regression budget should be tight (≤2 pp).
6. **Audio encoder on E4B (~300M)** — we should plan to disable/freeze it during LoRA, both to avoid touching modality-specific weights and to save memory.

**Confidence**: Cross-referenced HF + Google AI cards + GitHub issue + community search. Chat template format verified.

---

### Gemma 4 — HF blog post (April 2026) ⭐ this is our actual base

**Citation**: HuggingFace blog "Welcome Gemma 4: Frontier multimodal intelligence on device". Released by Google DeepMind on 2026-04-02.

**Sizes**:

| Model    | Effective | With embeddings | Context | Notes |
|----------|-----------|-----------------|---------|-------|
| E2B      | 2.3B      | 5.1B            | 128K    | base + IT; nested inside E4B (MatFormer) |
| **E4B**  | **4.5B**  | **8B**          | **128K**| **base + IT — our target** |
| 26B-A4B  | 4B active | 26B (MoE)       | 256K    | base + IT |
| 31B      | 31B dense | 31B             | 256K    | base + IT |

Training token counts not in blog. AIME 2026: **E2B 37.5%, E4B 42.5%, 26B-A4B 88.3%, 31B 89.2%** (vs Gemma 3 27B 20.8% — major reasoning lift). MMLU Pro: E4B 69.4%, 31B 85.2%.

**Architectural changes that affect fine-tuning**:
- **Per-Layer Embeddings (PLE)**: parallel low-dim conditioning pathway alongside the main residual stream. Each token gets a small dedicated vector per layer (token-identity + context-aware). Lightweight residual block after attention/FFN. **Originally introduced in Gemma 3n; now in all Gemma 4 sizes.** New parameter group → LoRA strategy needs review.
- **Shared KV Cache**: last N layers reuse K/V from earlier layers. Late-layer K/V projections are essentially tied. Affects what you LoRA on K/V.
- **Alternating local sliding-window (512–1024) + global full-context attention** with **dual RoPE** (standard for sliding, pruned for global).
- **MatFormer (Matryoshka Transformer)**: E2B's weights are a strict subset of E4B's. **Fine-tuning E4B implicitly affects the E2B subset.** Useful for deployment (one tune, two sizes); confounding for ablations.

**License**: **Apache 2.0** — major change from Gemma terms. Permissive commercial use, redistribution allowed.

**Chat template**: **NOT in blog**. Must verify via `transformers.AutoTokenizer.apply_chat_template` or the model card. Default assumption: same as Gemma 3 (`<bos><start_of_turn>user/model<end_of_turn>`). **Must confirm before training.**

**Tokenizer**: NOT explicitly described. Likely the same Gemma 3 256k SentencePiece base. **Must confirm.**

**Recommended FT recipes**: Blog shows TRL SFT example and Vertex AI Custom Training example. **No LR/batch/rank defaults given.** Frozen vision/audio towers in the Vertex AI example — consistent with our plan to LoRA the language path only.

**Quantization**: GGUF (llama.cpp) + 4-bit MLX (`mlx-community/gemma-4-26b-a4b-it-4bit`) tested. NF4/int4/fp16/bf16 specifics not detailed. **MLX path is what's already in `/Volumes/T9/gemma-god/eval/mlx-community__gemma-4-e4b-it-bf16/` — confirms the user's local setup matches what's distributed.**

**Takeaway for us**:
1. **Apache 2 unblocks production deployment.** Removes the legal friction that made Gemma less attractive than Mistral / Qwen.
2. **PLE is the new architectural concern for LoRA.** Standard `q,k,v,o,gate,up,down` may not cover everything that matters. Default for v0.3: leave PLE frozen; ablate later.
3. **Shared KV Cache means LoRA-ing late-layer K/V is wasted compute.** May want to constrain target_modules to early/mid layers explicitly.
4. **MatFormer is a deployment feature for us.** Tune E4B once → deploy on E4B (4.5B) or E2B (2.3B) subset depending on hardware. Helpdesk PC = E4B; Pi 5 = E2B subset.
5. **AIME 2026 E4B 42.5% means baseline reasoning is much stronger than Gemma 3 4B.** GSM8K kill switch threshold may need to be tighter (or we may have more headroom).
6. **Chat template + tokenizer verification is gating.** Cannot start training before confirming.

**Confidence**: Read in detail. Blog is authoritative on architecture; light on FT specifics.

---

### 2403.06350 — IndicLLMSuite / AI4Bharat (Sangraha + IndicAlign-Instruct)

**Citation**: Khan et al., 2024. arxiv:2403.06350v2. AI4Bharat. Code: github.com/AI4Bharat/IndicLLMSuite.

**Why it matters**: This is the most comprehensive Indic data infrastructure available. Gives us pretraining + instruction data for Nepali at production scale.

**Sangraha (pretraining)** — 251.3B tokens across 22 Indic languages. **Nepali (`npi`): 12.9B tokens total**:

| Slice | Tokens | What it is |
|-------|--------|------------|
| Sangraha Verified | **1.8B** | Manually curated, native-quality |
| Sangraha Synthetic | 10.6B | Machine-translated from English |
| Sangraha Unverified | 0.5B | Filtered from existing corpora |

Nepali ranks 7th by token volume (Hindi 34.5B, Bengali 30.0B lead). Sources: web (1.8B), PDF (10.6B), speech (0.5B). Nepali perplexity threshold = 120.32 (80th percentile) — they filtered ~41% of source.

**IndicAlign-Instruct (instruction)** — 74.7M total prompt-response pairs across ~20 Indic languages. Sources:

| Source            | Examples (all langs) | Type |
|-------------------|---------------------:|------|
| IndoWordNet       | 74.3M                | template-based, original |
| Wiki-Chat         | 202K                 | synthetic multi-turn (avg 9.14 turns) |
| Wiki-Conv         | 144K                 | synthetic conv (avg 2.8 turns) |
| Anudesh           | 43.3K                | crowdsourced native |
| HH-RLHF-Translated| 33K (approx)        | translated EN |
| Indic-ShareLlama  | 21.1K                | human prompts, model responses |
| WikiHow           | 20.3K                | translated |
| OpenAssistant-Tr  | 19.9K                | translated |
| Dolly-Translated  | 15.0K                | translated |

**Setu pipeline** = ensemble lang-ID (IndicLID + cld3 + NLLB) + 11 metrics + perplexity threshold + MinHashLSH dedup at 0.7 Jaccard. Documented quality gate.

**Nepali-specific in IndicAlign-Instruct**: Nepali is in the IndoWordNet (template) and translated subsets. Native (Anudesh) coverage not broken out by language in the table — likely Hindi-skewed.

**License**: "permissive licenses" claimed; specific names not in the excerpt.

**Takeaway for us**:
1. **Sangraha Verified Nepali (1.8B native tokens) is real fuel for any future CPT.** Bigger than we'd plausibly need for 4B-scale CPT-then-SFT.
2. **IndicAlign-Instruct gives us a Nepali instruction set without re-translating Alpaca.** Audit needed: how many examples are Nepali specifically, and what's the native-vs-translated split.
3. **Their Setu filtering pipeline is a recipe we should mirror** for our distilled tuples (lang-ID, perplexity gate, MinHash dedup).
4. **The IndoWordNet template-based examples (74.3M total)** are likely too narrow / repetitive for SFT — useful for vocabulary coverage but not task quality. Skip or downweight.
5. **No Indic-Gemma model trained on this corpus is publicly evaluated on Nepali.** Whoever does the Gemma 4 + Nepali combo first is publishable.

**Confidence**: Read in detail (HTML).

---

### Navarasa 2.0 — Indic Gemma 7B/2B for 15 Indian Languages (Telugu LLM Labs, March 2024)

**Citation**: Telugu LLM Labs (Ravi Theja, Ramsri Goutham Golla). Released March 18, 2024. Published as a Medium article, not a peer-reviewed paper.

**Setup**:
- Base: **Gemma 1 7B / 2B** (NOT Gemma 2 / 3 / 4 — predates them all).
- 15 Indian languages including Nepali.
- ~630K instruction samples translated from `alpaca-cleaned-filtered` (presumably via NLLB or Google Translate; not specified).
- Nepali-specific subset: `nepali_alpaca_yahma_cleaned_filtered`.
- HuggingFace: `Telugu-LLM-Labs/Indic-gemma-7b-finetuned-sft-Navarasa-2.0` (+ 2B variant + GGUF).

**Method / hyperparams**: **Not disclosed.** No LoRA rank, no LR, no batch, no epochs.

**Results**: **Not benchmarked.** Qualitative example outputs only.

**Takeaway for us**:
1. **Proof of life** — Gemma family has been Indic-fine-tuned with Nepali in scope. So it's not architecturally hopeless.
2. **Otherwise nearly useless as a recipe**: Gemma 1, no hyperparams, no eval, MT-only data, no anti-forgetting. Predates everything we know now (Gemma 2/3/4, GaMS-3, GemMaroc, Romanized Nepali paper).
3. **The author's own caveat** — "2B model outputs a bit inconsistent" — suggests the Gemma-1-2B + translated-Alpaca recipe ran into the same generation-collapse pattern Borsch / Marathi / CPT-v1 documented. Familiar shape.

**Confidence**: Read in detail (Medium article).

---

### 2501.13912 — Analysis of Indic Language Capabilities in LLMs (reality-check on Nepali)

**Citation**: Vaidya et al., 2025. arxiv:2501.13912v1.

**Key finding for us**: **Nepali is excluded from the core 12-language Indic ranking** (Table 4 of the paper). The paper prioritizes Indian Constitutional languages; Nepali is widely spoken but *not* an Indian Constitutional language, so it falls below the analytical threshold.

**Other points**:
- Indic = 0.5% of Common Crawl; 7.72% of mC4. Massive underrepresentation.
- Llama 3.1's 15T multilingual training included Hindi as **the only Indic language**.
- "Sharp decline in performance of extremely low resource languages" (Doddapaneni et al. 2023) for tail Indic languages.
- IndicGenBench covers 29 Indic languages — broader than other suites; we should check if Nepali is there.
- Token fertility varies sharply: Urdu lowest, Tibetan highest. **No Nepali-specific fertility number.**
- MuRIL (17 Indic + transliterations) was best on IndicXTREME — but Nepali coverage unclear.

**Takeaway for us**:
1. **Nepali is more low-resource than the typical "Indic" paper assumes.** Even when papers claim Indic coverage, Nepali is often dropped or token-thin. Calibrate expectations downward.
2. **There is no published Gemma-4-on-Nepali baseline.** When we run our experiment, we will be the baseline.
3. **IndicGenBench may include Nepali** — worth verifying for use as an open-domain Nepali generation eval.
4. **The data scarcity is the real constraint, not the algorithm.** Sangraha Verified Nepali (1.8B native) is among the largest curated Nepali corpora in existence.

**Confidence**: Read in detail (HTML).

---

### Tier-3 abstracts (read for v0.3 hyperparam confirmation)

**QLoRA (2305.14314, Dettmers et al.)** — abstract only:
- 65B fine-tunable on a single 48GB GPU.
- Guanaco reaches "99.3% of ChatGPT" with 24h on a single GPU.
- NF4 = "information-theoretically optimal for normally-distributed weights"; double quantization (compresses the quantization constants); paged optimizers prevent memory spikes.
- HTML version 404'd; full numbers not extracted. **For v0.3 we treat NF4 + 4-bit base + 16-bit adapters as standard.**

**DoRA (2402.09353, Liu et al., ICML 2024)** — abstract only:
- Decomposes pretrained weight into magnitude + direction. LoRA on direction; magnitude learned independently.
- "Consistently outperforms LoRA on LLaMA, LLaVA, VL-BART" on commonsense reasoning, visual instruction tuning, image/video-text understanding.
- No additional inference overhead.
- **Decision for v0.3**: rsLoRA already chosen and is a different axis (scaling, not decomposition). DoRA-vs-rsLoRA is an ablation we can run if v0.3 underperforms; not for first run.

**Secret Recipe (2412.13337, Pareja et al., Dec 2024)** — abstract only:
- 3B-7B SFT focus. **Larger batches + lower LR > smaller batches + higher LR.**
- "Lower gradient norms and higher loss values [in early training]" predict better final performance — usable as an early-stop signal.
- No significant difference between phased and stacked training; stacked is simpler.
- Challenges TULU and Orca recommendations.
- **Decision for v0.3**: Our LR (1e-4) is on the low side, batch (16 effective) is moderate. Acceptable. May want to ablate larger batch with lower LR if we have compute.

**Confidence**: Abstracts only. HTMLs accessed inconsistently.

---

### 2503.19786 — Gemma 3 Technical Report (Google DeepMind, March 2025)

**Citation**: Gemma Team / Google DeepMind, March 2025. arxiv:2503.19786.

**What's relevant for us**:
- **Tokenizer**: 256k entries, SentencePiece (split digits, preserved whitespace, byte-level encodings). "Same as Gemini 2.0", "more balanced for non-English." **No specific Devanagari/Nepali stats disclosed.** Need to measure ourselves.
- **Sizes & training tokens**: 1B (2T), 4B (4T), 12B (12T), 27B (14T). For comparison: Gemma 2 9B was 8T (KD); Llama 3 8B was 15T. **Gemma 3 4B at 4T is well-trained relative to its size.**
- **Architecture**: 128K context (1B: 32K). 5:1 local:global attention. RoPE base 1M (global), 10k (local). QK-norm replaces soft-capping (Gemma 2 → 3 change).
- **Chat template (CRITICAL — exact format)**:
    ```
    <start_of_turn>user
    {text}
    <end_of_turn>
    <start_of_turn>model
    {response}
    <end_of_turn>
    ```
    `[BOS]` token must be prepended (token, not literal string). Output terminates with `<end_of_turn>` for IT models. **If we tune from gemma-3-*-it, our SFT data must use this exact format. Wrong format = chat collapse — same failure mode as CPT v1.**
- **Multilingual benchmarks** (Gemma 3 27B-IT):
  - MGSM: 74.3% (Gemma 2 27B: 68.0%) — multilingual math improved
  - Global MMLU-Lite: 75.1%
  - **IndicGenBench: 63.4%** (Gemma 2 27B: 62.1%) — modest Indic gain
- **Pre-training data**: Multilingual data "increased" with monolingual + parallel; Chung et al. 2023 sampling for fairer balance. **No Nepali share disclosed.**
- **Post-training**: SFT from synthetic prompts/responses (KD from larger teacher) → RL with BOND/WARM/WARP. Heavy filtering for "mistaken self-identification." **Mirrors GaMS-3's identity-correction step — confirms it as standard practice.**

**Takeaway for us**:
1. **Use Gemma 3 4B-IT (or 12B-IT) as the base.** Tokenizer claims plus IndicGenBench 63.4% support trying it on Nepali.
2. **Chat template is non-negotiable.** Wrong formatting at SFT time is a one-line bug that silently destroys results.
3. **No fine-tuning recipe in the report** — synthesize from external papers (GaMS-3, GemMaroc, Romanized Nepali).
4. **Gemma 3 4B's 4T training tokens give it enough capacity** that SFT (without CPT) should be the right starting move — provided Devanagari coverage is adequate (verify).

**Confidence**: Read in detail (HTML).

---

### 2408.00118 — Gemma 2 Technical Report

**Citation**: Gemma Team / Google DeepMind, August 2024. arxiv:2408.00118.

- Tokenizer 256,128 entries (matches Gemma 3 — same SentencePiece base).
- **Same chat template as Gemma 3**: `<bos>` + `<start_of_turn>user|model` + `<end_of_turn>` + `<eos>`.
- **Gemma 1 → 2 change**: model now explicitly outputs `<end_of_turn><eos>`, not just `<eos>`. Important for serving / streaming.
- Sizes: 2B (2T tokens, KD), 9B (8T, KD), 27B (13T, from scratch).
- Post-training: SFT (synthetic + real) → **RLHF with reward model 10× larger than policy** → **model averaging across runs**. LMSYS-chat-1M used for prompts only.
- **No fine-tuning hyperparams provided.**

**Takeaway**:
1. Chat template is consistent across Gemma 2 → 3. Whatever format we lock in works for either base.
2. **Model averaging across runs is a free anti-forgetting trick** Google used at the source level. We can mimic by averaging multiple LoRA seeds at the end.

**Confidence**: Skimmed for fine-tuning-relevant sections.

---

### 2404.09138 — From Bytes to Borsch (Ukrainian Gemma + Mistral)

**Citation**: Kiulian, Polishko, Khandoga, Chubych, Connor, Ravishankar, Shirawalmath. April 2024.

**Setup**:
- Bases: Gemma 2B-IT, Gemma 7B-IT, Mistral-7B-Instruct-v0.1.
- Target: Ukrainian (Cyrillic, low-resource Slavic).
- **UKID-v0.1: 962 native QAF triples** generated via Gemini 1.0 from Ukrainian Wikipedia. **Native, not MT.** Filtered Wikipedia by traffic (3K-150K/month) → 367 relevant pages → 962 triples.
- Bigger ingredient: 10K UAlpaca (translated) + 3K ZNO (native exam).

**Method**: LoRA only.
- Gemma 2B/7B: r=4 (very low). 11M trainable on 2B (~0.5%).
- Mistral 7B: r=32, α=16, AdamW, 4×A100-80GB (Axolotl).
- 3–5 epochs sufficient for MCQ format adaptation.

**Data composition**:
- Gemma-7B: 71% UAlpaca (translated) + 22% ZNO + 7% UKID (native).
- **No English replay.** No anti-forgetting technique. No tokenizer extension.

**Results — same shape as our CPT v1**:

| Task                       | Metric    | Gemma 7B | Gemma 7B-FT | Δ           |
|----------------------------|-----------|----------|-------------|-------------|
| ZNO History MCQ            | accuracy  | 26.36%   | 37.96%      | **+11.6 pp** |
| Open-gen Ukrainian fluency | rating    | 85       | 54          | **−31 pts**  |
| Open-gen Grammar           | rating    | 35       | 19          | **−46% rel** |

**MCQ task improved (+11.6); open-generation Ukrainian collapsed (−31 pts).** Canonical low-resource adaptation failure — and **exactly what happened to CPT v1**.

**Other observations**:
- "Code-switching" emerged: spontaneous Ukrainian-Russian mixing (`Твiр про коллекцию кольоровых олiвцов`). **Direct analog to our Roman-NE / Devanagari mixing.**
- Authors acknowledge: translated Alpaca/Squad lacked "Ukrainian context and knowledge specific to cultural and historical aspects."

**Takeaway for us**:
1. **Borsch's failure replicates CPT v1's failure.** Different base, different language, same shape: format-task improves, generation-task collapses.
2. **>90% MT data + no English replay → generation collapse.** Borsch had 7% native + 0% English replay and lost 31 pts of fluency. CPT v1 had 9.5% instruction format and lost chat behavior. **Same disease.**
3. **Code-switching is a known low-resource failure mode**, not Nepali-specific. Mitigation must be in the data design.
4. **r=4 LoRA was insufficient for Gemma adaptation.** Romanized Nepali's r=32 is much better. Don't go below r=32.

**Confidence**: Read in detail (HTML), tables extracted.

---

### 2510.04139 — Tsinghua Final Report: Low-Resource Fine-Tuning (Swedish, Gemma 2 2B)

**Citation**: Johansson, Bakkenes, Wang. Fall 2024 ML course project, Tsinghua. arxiv:2510.04139v1.

**What it is**: A course project, not peer-reviewed. Light experimental scaffolding, but one finding is directly load-bearing for us.

**Setup**: Gemma 2 2B-IT, Swedish, LoRA r=8, 500 hand-crafted samples, 10 epochs (early stop p=3), batch 16, cosine + warmup, AdamW, seq-len 128, mixed precision.

**Headline result**:

| Pipeline           | Swedish QA F1 |
|--------------------|---------------|
| Pretrained (baseline) | 64.98% |
| Fine-tuned only    | **47.72%** (regressed!) |
| Fine-tuned + RAG   | **77.63%**     |

**Fine-tuning alone made the model WORSE on QA (−17 pp).** Adding RAG more than recovered (+13 vs baseline). Also: ROUGE-1 0.69 → 0.76 on summarization; BLEU 0.29 → 0.46 on translation — fine-tuning helped formatting but hurt knowledge QA until RAG was added.

**Takeaway for us**:
1. **Validates our RAG-first architecture.** A 500-sample LoRA on Swedish Gemma 2 2B regressed QA F1 by 17 pp; with RAG it climbed to 77.63 (+29 from raw FT, +13 from baseline). For knowledge-intensive tasks **RAG is load-bearing; fine-tuning is the icing.**
2. **Don't expect SFT alone to hit our groundedness target.** RAG retrieval + tuned composer working together is the architecture. Tuning the composer in isolation is the wrong experiment.
3. **Course-project quality** — take effect sizes with caveats. Directional finding aligns with everything else.

**Confidence**: Read in detail (HTML).

---

### 2312.03732 — rsLoRA: Rank-Stabilized Scaling Factor

**Citation**: Damjan Kalajdzievski, November 2023. arxiv:2312.03732.

**Mechanism**: Standard LoRA scales weight updates by α/r. At high rank (r ≥ 32) this attenuates gradient signal — variance of ΔW collapses as r grows. rsLoRA replaces α/r with **α/√r** — keeps gradient norms stable as rank scales.

**Practical implication**:
- Standard LoRA: rank > 8 gives diminishing returns (the common reason "high rank doesn't help").
- rsLoRA: high ranks (32, 64, 128) recover expected scaling — more capacity, better fine-tuning, same inference cost (LoRA adapters merge into base regardless of rank).

**Takeaway for us**:
1. **rsLoRA at r=32 is the right call.** Romanized Nepali used it; mechanism is sound. Standard LoRA at r=32 leaves capacity on the table.
2. **No new hyperparams** — single scaling change at adapter init. Modern frameworks support it as a flag (`use_rslora=True` in PEFT, Unsloth, etc.).

**Confidence**: Read abstract + key claims; mechanism understood.

---

### 2604.14171 — Romanized Nepali benchmarking ⭐ (direct relevance)

**Citation**: Rimal & Rimal, March 2026. Nepal Engineering College + Tribhuvan University. arxiv:2604.14171v1.

**Setup**:
- Base models: **Llama-3.1-8B, Mistral-7B-v0.1, Qwen3-8B**. No Gemma.
- Data: `Saugatkafley/alpaca-nepali-sft` (Devanagari, 52K), transliterated to Roman via AI4Bharat Indic-Transliteration. 9K train / 1K test.
- Two pathways:
  1. Semantic Translation: English instruction + Roman-NE input/output
  2. Full Phonetic Transliteration: all fields Roman-NE with intentional orthographic variation (`chha`/`cha`)
- Eval: PPL, BERTScore, chrF++, ROUGE-1/2/L, BLEU + 10 hand-picked Golden Questions.

**Method**: QLoRA NF4 4-bit + **rsLoRA** (rank-stabilized: scaling = α/√r, not α/r — preserves gradient norms at high rank).
- r=32, α=64, target = `q_proj, k_proj, v_proj, o_proj, gate_proj, up_proj, down_proj` (full attention + MLP).
- LR 1e-4, AdamW 8-bit, warmup 200 steps, 3 epochs (3,375 steps), batch 16 effective.
- Hardware: 2×T4 16GB (consumer-tier — fits anywhere).
- Total compute: ~26h26m total wall, ~8-9h per model.
- Trainable params: ~83-87M (≈1% of base).

**Results** (PPL ↓, others ↑):

| Model            | ZS PPL | Post PPL | BERTScore | chrF++ | ROUGE-L | BLEU   | Errors/10 |
|------------------|--------|----------|-----------|--------|---------|--------|-----------|
| Llama-3.1-8B     | 52.79  | 3.024    | **0.7511**| 26.97  | 0.2359  | 0.0498 | 0/10      |
| Qwen3-8B         | 27.89  | 2.946    | 0.7505    | **27.47**| **0.2511**| **0.0550** | 0/10  |
| Mistral-7B-v0.1  | 27.53  | **2.812**| 0.7339    | 23.95  | 0.2144  | 0.0404 | 2/10      |

Zero-shot failure modes: Llama 6/10 null (early EOS), Mistral 5/10 newline loops, Qwen 0/10 null but all in English/Devanagari (script drift).

**Tokenizer finding**: Llama Tiktoken initial loss 2.10 vs Mistral SentencePiece 1.90 — Tiktoken over-fragments Romanized Nepali into low-frequency subwords.

**Takeaway for us**:
1. **rsLoRA r=32, α=64, LR 1e-4, 3 epochs is a directly applicable recipe.** Proven on Romanized Nepali. Fits on a T4. We should run this exact config on Gemma 3 4B as our first experiment — fills a literature gap.
2. **`Saugatkafley/alpaca-nepali-sft` (52K Devanagari) is real Nepali instruction data.** Open question: how much is native-quality vs auto-translated Alpaca? Need to verify.
3. **Their "0/10 errors on Golden Questions" claim is weak eval.** No interrater agreement, hand-picked questions. Reinforces task #32 as a real eval set.
4. **Tokenizer can be 5-10% of the loss curve.** Gemma 3 claims "wide multilingual coverage" — need to measure bytes/token on Nepali Devanagari.
5. **rsLoRA > vanilla LoRA at r=32.** Not yet read the rsLoRA paper itself but it's used here without ablation.

**Confidence**: Read in detail (HTML), full tables extracted.

---

### 2603.01691 — GaMS-3: Slovene Gemma 3 12B (full-FT CPT + IT)

**Citation**: Vreš et al., March 2026. University of Ljubljana. arxiv:2603.01691v1.

**Setup**:
- Base: Gemma 3 12B (pre-trained version).
- Target: Slovene (~2M speakers, low-resource Indo-European).
- Hardware: LEONARDO Booster (128 nodes × 4×A100), FRIDA cluster, NVIDIA DGX Cloud Lepton.

**CPT** (3 stages, **full FT**, ~140B tokens):

| Stage              | Tokens   | Data sources                                              | LR (max) | Batch         |
|--------------------|----------|-----------------------------------------------------------|----------|---------------|
| Parallel alignment | ~12.8B   | DGT, MaCoCu, KAS, Wikipedia (MT)                          | 5e-6     | 128           |
| Base CPT           | ~100.9B  | Nemotron EN (~27%), FinePDFs SR/HR/BS (~32%), SL (~41%)   | 5e-6     | 128 → 192 → 256 |
| Long CPT (131k ctx)| ~20.1B   | Nemotron subset, FinePDFs remainder, Trendi, Math (translated) | 5e-6 | 256           |

Anti-forgetting: ~27% English replay through base CPT. Parallel-alignment stage uses MT data as a distribution bridge.

**SFT** (2 stages, **full FT**, ~196K examples):
- Stage 1 (general IT): 107.6K (~68k Slovene + ~40K English) — ClosedQA, OpenQA, Writing, Digital Humanities, Math.
- Stage 2 (chat IT): 88.6K, **80% Slovene / 20% English** — GaMS-Nemotron-Chat (LMSYS Chat 1M translated by GaMS-27B → MinHash-deduped at 0.65, **identity-corrected** to remove "Qwen" self-references).
- LR 5e-6, batch 64 (microbatch 8), 2 epochs, ZeRO-2.

**Tokenizer**: Used Gemma 3 tokenizer as-is. **No vocabulary extension.**

**Results** (Slovene-LLM-Eval, GaMS3-12B vs Gemma 3 12B base):

| Benchmark         | GaMS3-12B | Gemma 3 12B | Δ         |
|-------------------|-----------|-------------|-----------|
| ARC-Challenge     | 0.5265    | 0.4514      | +7.5%     |
| ARC-Easy          | 0.7744    | 0.6936      | +8.1%     |
| HellaSwag         | 0.5111    | 0.4728      | +8.1%     |
| OpenBookQA        | 0.3940    | 0.3520      | **+11.9%**|
| PIQA              | 0.7149    | 0.6616      | +8.1%     |
| Winogrande        | 0.7056    | 0.6559      | +7.6%     |
| BoolQ             | 0.8523    | 0.8526      | tie       |
| **GSM8K (5-shot)**| 0.6892    | 0.7430      | **−5.4% regression** |
| **TruthfulQA MC1**| 0.3807    | 0.3978      | **−4.3% regression** |

EN→SL translation: COMET 0.700 vs 0.613. Language-error rate 1.11% vs 16.48%. Big production-relevant win.

Slovene LLM Arena ELO: 1025 vs Gemma 3 12B 979 (+46). Comparable to GPT-4o (1030).

**Failure modes flagged**:
- Few-shot reasoning regressed (GSM8K, TruthfulQA — both 5-shot).
- "Slovene appears slightly more machine-translated" than other models (MT artifact).
- 32.6% markdown errors in EN→SL translation (vs 3.7% Gemini).
- No multimodal use of Gemma 3 yet.

**Takeaway for us**:
1. **Full-FT CPT works for Gemma 3 12B + low-resource Indo-European, but compute is enormous** (~140B tokens, 128-node A100 cluster). **Out of reach for us.**
2. **~27% English in CPT is the operating point for not-starving the original distribution.** CPT v1's ~9.5% instruction-format was both wrong axis and wrong proportion.
3. **Chat data dominated their result.** Without GaMS-Nemotron-Chat, prior GaMS-9B-Instruct ranked worst. **Chat preservation is not optional.**
4. **GSM8K still regressed −5.4% even with 140B-token CPT.** Few-shot reasoning is fragile at any scale. Set the budget upfront.
5. **No tokenizer extension; trusted Gemma 3's existing tokenizer.** One less thing to do (provided we measure it first).
6. **"Identity correction" step**: GaMS-27B-translated chats had model self-identifying as Qwen. **Anyone distilling from Sonnet/Kimi must filter teacher self-references** — concrete operational note.

**Confidence**: Read in detail (HTML), full tables extracted.

---

### 2505.17082 — GemMaroc: Darija via Gemma 3 4B/27B (LoRA, 5K instructions)

**Citation**: Skiredj, Azhari, Atou, Tazi, Berrada, 2025. arxiv:2505.17082.

**Setup**:
- Base: Gemma 3 4B and 27B.
- Target: Moroccan Arabic (Darija).
- Data: 5K mixed instructions, **all autotranslated via Gemini 2.0 Flash API**.
  - LIMA 1K → 700 Darija + 300 English
  - DEITA 6K → 3.7K Darija + 1.3K English
  - TULU 50K → 33K Darija + 13K English (this is the bulk)
- 20% English preserved across each suite "to avoid catastrophic forgetting."

**Method**: LoRA only.
- 4B: r=32, α=64, LR 4e-4 (LIMA, DEITA), 1e-4 (TULU). Epochs 15/6/3.
- 27B: r=16, α=32, LR 1e-4 (TULU).
- bf16, max-seq 2048.
- Compute: 4B = 10 GPU·h on 2×A100; 27B = **48 GPU·h on 8×H100** (6h wall).

**Results**:

| Model               | DarijaMMLU | DarijaHellaSwag | GSM8K   | MMLU-en |
|---------------------|------------|-----------------|---------|---------|
| Gemma 3 4B (base)   | 32.8%      | 36.3%           | 74.75%  | 51.1%   |
| GemMaroc-4B-TULU    | 47.5%      | 47.1%           | **55.95%** ⚠️ | 73.95%  |
| Atlas-Chat-27B      | 61.95%     | 48.4%           | 82.0%   | 72.1%   |
| **GemMaroc-27B**    | 61.6%      | **60.5%**       | 84.2%   | 79.4%   |

⚠️ **4B regressed −18.8 pp on GSM8K**. The paper does not dwell on this. The 27B held — capacity absorbs the shock.

The "native > autotranslated" claim is **cited from related work, not tested here**. All 5K samples are Gemini-MT.

**Takeaway for us**:
1. **5K instructions can move DarijaMMLU +14.7 pp.** Instruction-only is a real lever for the *target language*.
2. **20% English preservation = sole anti-forgetting trick.** No KL anchor, no replay buffer, no STM. This is the simplest workable recipe.
3. **Capacity matters for math.** 4B regressed −18.8 pp on GSM8K; 27B held. **If we tune Gemma 3 4B for production, expect math collapse unless we mitigate.**
4. **TULU's reasoning-dense English prompts lifted MMLU-en (51 → 74).** Counterintuitive — a 30% English share of a 5K mix improved English benchmarks. Likely instruction-following format more than language gain.
5. **Their "Green AI" framing matches a low-cost recipe.** 10 GPU·h for 4B, 48 for 27B. Reproducible without a research cluster.

**Confidence**: Read in detail (HTML), full tables extracted.

---

### 2411.18571 — Marathi via Gemma LoRA PEFT (eval-methodology critique)

**Citation**: Khade, Jagdale, Phaltankar, Takalikar, Joshi, Nov 2024. arxiv:2411.18571.

**Setup**:
- Base: **Gemma 1 2B + 2B-it; Gemma 2 2B + 2B-it.** All 2B — no 7B in the actual paper despite Claude's listing.
- Target: Marathi.
- Data: 52K Alpaca-Marathi (Google Translate from English). No quality filter.

**Method**: LoRA. **Hyperparameters not disclosed** — no rank, alpha, target modules, LR, batch, steps. This is a paper-hygiene issue.

**Results** (F1 — selected):

| Benchmark      | gemma-2-2b-it | gemma-2-2b-it (Mr) | Δ          |
|----------------|---------------|---------------------|------------|
| IndicSentiment | 0.9749        | 0.9589              | mild       |
| ARC-Easy       | 0.6851        | 0.6343              | −7%        |
| ARC-Challenge  | 0.7210        | 0.6374              | −12%       |
| Indic COPA     | 0.7210        | 0.5835              | −19%       |
| **Indic XNLI** | 0.2814        | 0.1667              | **−41% relative** |

Manual eval: 150 questions, **rater count unspecified, no agreement metric**. Hand-wave claim that fine-tuned wins.

**Takeaway for us**:
1. **Translated Alpaca + LoRA on a 2B model + no replay = 41% reasoning regression.** The closest direct analog to a CPT-v1-style failure with a different mechanism (instruction-only at SFT instead of raw text at CPT). The damage is the same shape.
2. **Indic XNLI is a usable Nepali reasoning regression metric.** Belebele tests *general* Nepali but not reasoning; XNLI does. Worth borrowing.
3. **"Manual eval suggests improvement" without rater count or agreement is not eval.** This is the paper they cite as evidence for a methodology gap, but they themselves commit the methodology error.

**Confidence**: Read in detail (HTML).

---

### 2501.14315 — STM (Selective Token Masking) via Low-Perplexity Filtering

**Citation**: Wu, Tam, Lin, Chen, Sun, Lee. Jan 2025. arxiv:2501.14315. NeurIPS 2025.

**Setup**:
- Models: **Gemma 2 IT 2B**, Llama 3 8B Instruct, +3 others.
- Tasks: MBPP (Python), MATH, BIRD (text-to-SQL). Non-target retention: GSM8K, ARC-Challenge.
- **All English domain transfer. No multilingual or language-adaptation experiments.**

**Method**:
- Compute per-token perplexity on the ground-truth training sequence using the pre-fine-tuning model.
- Mask (zero out loss for) tokens where PPL > τ. Optimal τ = 2.5 (filters ~20-24% of tokens).
- LR 2e-5 (primary). LoRA + DoRA + full FT all tested.
- ~300 steps ≈ 5–6 epochs.

**Results** (Gemma 2 IT 2B, key entries):

| Task | Setup        | BWT (non-target retention) | TI (target gain) |
|------|--------------|----------------------------|-------------------|
| MBPP | baseline     | −38.19%                    | −21.76%           |
| MBPP | STM (τ=2.5)  | **+0.42%**                 | **0.00%**         |
| MATH | baseline     | −36.68%                    | −22.78%           |
| MATH | STM (τ=2.5)  | **−2.93%**                 | **+7.83%**        |

Mechanism: high-PPL tokens require larger weight updates → those updates are what overwrite other capabilities.

**Takeaway for us**:
1. **STM is the literal countermeasure to CPT v1's failure mode** — high-PPL tokens are the ones that produce capability-overwriting updates. The mechanism is right.
2. **But for language adaptation, "high-PPL tokens" map to "rare/foreign vocabulary"** — exactly the Devanagari Nepali signal we want to learn. **STM might mask too much and starve the language signal.** Not a slam-dunk transfer.
3. **Worth an ablation**: Gemma-Nepali SFT with vs without STM masking at τ=2.5. If non-target benchmarks (Belebele-en, GSM8K-en) hold without sacrificing Nepali gain, it's a free win.
4. **STM is per-token, not per-sequence** — useful for instruction tuning where prompt is low-PPL but assistant output may be high-PPL.
5. **Not validated above 10B.** Gemma 3 12B is in scope; 27B less certain.

**Confidence**: Read in detail (HTML), exact numbers extracted.

---

## Cross-cutting synthesis (Pass 1)

### Pattern 1: GSM8K is the canary; math/few-shot reasoning regresses first

| Paper             | Base    | Method   | GSM8K Δ                    |
|-------------------|---------|----------|----------------------------|
| GemMaroc 4B       | Gemma 3 4B | LoRA  | 74.75 → 55.95 (−18.8 pp)   |
| GemMaroc 27B      | Gemma 3 27B | LoRA | held at 84.2              |
| GaMS-3 12B        | Gemma 3 12B | full-FT CPT 140B | 0.7430 → 0.6892 (−5.4%) |
| Marathi 2B-it     | Gemma 2 2B | LoRA  | not measured; XNLI −41%, COPA −19% |

**Implication for us**: set a GSM8K-en regression budget upfront (e.g., ≤2 pp) as a kill switch. **At 4B, expect math damage even with conservative recipes.**

### Pattern 2: ~20% English replay is the convergent operating point

| Paper        | English mix                                          |
|--------------|------------------------------------------------------|
| GemMaroc     | 20% across each instruction suite                    |
| GaMS-3 chat  | 80/20 Slovene/English                                |
| GaMS-3 base CPT | ~27% English (Nemotron) of ~140B                  |
| Marathi      | 0% — and reasoning regressed −41%                    |
| **CPT v1 (us)** | ~9.5% instruction-format. Was the wrong axis and wrong proportion. |

**Implication**: For SFT, 20% English mix. For CPT, 25-30%. **CPT v1 had ~half of what working recipes use.**

### Pattern 3: MT-only data is the universal limitation; nobody has run native-vs-MT controlled

- GemMaroc: 100% Gemini 2.0 Flash MT
- Marathi: 100% Google Translate MT
- Romanized Nepali: 100% rule-based transliteration
- GaMS-3: GaMS-27B-translated LMSYS for chat data

All four explicitly cite native > MT in related work but do not run the comparison themselves.

**Implication**: Our distillation plan (Sonnet/Kimi-generated answers grounded on retrieved Nepali chunks) is essentially MT-with-context. **Whether that's better than pure Alpaca-MT is an open empirical question we can answer with a small-scale ablation (1K vs 1K).**

### Pattern 4: Tokenizer matters; Tiktoken over-fragments Romanized scripts

- Llama-3.1 (Tiktoken): 2.10 initial loss on Romanized Nepali
- Mistral / Qwen (SentencePiece): 1.90
- Gemma 3: 256k SentencePiece (same as Gemini 2.0), claimed multilingual balance — but **Gemma 3 tech report discloses no Nepali/Devanagari stats**. Their MGSM result improved to 74.3% (vs Gemma 2 68.0%) with no per-language breakdown.
- **None of our recipe sources extended the vocabulary**: GaMS-3 (Slovene), GemMaroc (Darija), Borsch (Ukrainian), Romanized Nepali all trusted Gemma's tokenizer as-is.

**Implication**: Measure Gemma 3 4B's bytes-per-token on a Nepali Devanagari paragraph before training. If > 2.0 average, consider extension. If < 1.5, trust as-is. **Default is trust as-is unless our measurement disagrees.**

### Pattern 5: Eval methodology gap is real; task #32 is correctly scoped

- Marathi: explicit gap between automatic decline and manual claim, but manual eval has no rater count or agreement
- GemMaroc: DarijaMMLU is itself MT-based — translation-bias contamination
- GaMS-3: only 4 of 9 Slovene benchmarks were manually corrected by native speakers

**Implication**: A native-Nepali groundedness eval (task #32) is **not optional**. Belebele and FLORES are necessary but insufficient. Build the eval set first.

### Pattern 6: Full-FT CPT requires research-cluster compute

GaMS-3 used 128 A100 nodes for 140B tokens. We don't have that. GemMaroc and Romanized Nepali both got useful results with QLoRA on 2-8 GPUs. **For us, PEFT on instruction data is the only feasible direction unless we pivot to a much smaller CPT scope.**

### Pattern 7: Format-task improves while generation-task collapses (the universal CPT v1 shape)

| Paper                  | Format-task gain          | Generation-task regression                       |
|------------------------|---------------------------|--------------------------------------------------|
| Borsch Gemma 7B        | ZNO history MCQ +11.6 pp  | Open-gen fluency −31 pts, grammar −46% rel       |
| Marathi Gemma 2 2B-it  | IndicSentiment ~ held     | Indic XNLI −41% rel, COPA −19%                   |
| GemMaroc 4B            | DarijaMMLU +14.7 pp       | GSM8K −18.8 pp                                   |
| **CPT v1 (us)**        | (raw text adaptation)     | chat behavior collapsed                          |

**Implication**: The failure mode is universal across Gemma sizes (2B, 4B, 7B), languages (Ukrainian, Marathi, Darija, Nepali), and methods (LoRA, full FT). **Whatever recipe we run, our eval must include both a format/MCQ-style task AND an open-generation task.** A closed-form benchmark alone will report success while production-relevant generation has collapsed — which is exactly the CPT v1 failure documented in BENCHMARKS.md §3.2.

### Pattern 8: SFT alone often regresses on knowledge QA; SFT + RAG decisively wins

Tsinghua Swedish (Gemma 2 2B): pretrained QA F1 64.98% → fine-tuned 47.72% (−17 pp) → fine-tuned + RAG 77.63% (+13 vs baseline).

**Implication**: For our knowledge-intensive helpdesk task, fine-tuning the composer in isolation is the wrong experiment. **The composer must be evaluated in the RAG pipeline against the groundedness eval, not as a standalone QA model.**

### Pattern 9: Gemma 4 changes the recipe in concrete ways (PLE, Shared KV, MatFormer, Apache 2)

| Change                  | Effect on our recipe                                                                       |
|-------------------------|--------------------------------------------------------------------------------------------|
| **Apache 2.0 license**  | Production deployment unblocked. Was a friction point with Gemma terms.                    |
| **Per-Layer Embeddings**| New parameter group alongside residual stream. Standard `q,k,v,o,gate,up,down` LoRA targets may not cover it. **Open question: LoRA PLE or freeze?** |
| **Shared KV Cache**     | Late-layer K/V projections tied to earlier layers. LoRA-ing them late is wasted capacity.  |
| **MatFormer**           | E2B is a strict subset of E4B's weights. Tuning E4B implicitly tunes E2B subset — deployment win (one tune, two sizes).                       |
| **AIME 2026: 42.5% on E4B** | Reasoning baseline far exceeds Gemma 3 4B. GSM8K-en kill switch may need calibration.   |
| **Chat template**       | NOT disclosed in HF blog. Must verify whether it matches Gemma 3's `<bos><start_of_turn>...`. **Gating before training.** |

**Implication**: Recipe v0.2 (designed for Gemma 3 4B-IT) needs three concrete adjustments before execution: (a) verify chat template, (b) decide PLE LoRA strategy, (c) treat MatFormer nesting as a deployment feature.

### Pattern 10: Existing Indic Gemma work doesn't measure Nepali rigorously; we will be the baseline

| Resource                | What it gives us                                                          | Gap                                                  |
|-------------------------|--------------------------------------------------------------------------|-----------------------------------------------------|
| Navarasa 2.0 (2024)     | Proof Gemma can be Indic-tuned with Nepali in scope; HF model available  | Gemma 1, no hyperparams, no Nepali eval, MT-only data |
| IndicLLMSuite (2024)    | **12.9B Nepali pretraining tokens** + IndicAlign-Instruct (Nepali in ~20 langs); Setu filtering pipeline; permissive license | No publicly evaluated Indic-Gemma trained on it      |
| Indic Capabilities (2025)| Survey confirming Nepali is below the 12-language analytical threshold  | Constitutional-Indian-language bias; Nepali under-coverage |

**Implication**: Nepali is more low-resource than typical "Indic" research assumes. The data infrastructure exists (Sangraha + IndicAlign-Instruct), but **no public Gemma 4 + Nepali baseline.** Our experiment fills a real gap; correspondingly, we should not expect to inherit a mature recipe.

### Pattern 11: Mixed-source data (native + translated + synthetic) decisively beats single-source for low-resource

| Paper                       | Single-best source         | Mixture-best                          |
|-----------------------------|----------------------------|----------------------------------------|
| MedInjection-FR (controlled, Qwen-4B) | NAT only 47.13 EM | NAT+TRAD 49.24 EM (+2.1) **with half the native data** |
| MURI (mT5-XXL, 200 langs)   | mT0 31.5% MMLU             | MURI native-output 36.0% (+14% rel)    |
| Lebanese (Aya23-8B)         | LW native 3K → 74.4        | (single-source experiment; quality-over-quantity confirmed) |
| GemMaroc (Gemma 3 4B)       | TULU 47.5 DarijaMMLU       | (with 20% EN preserved) — implicit mix |

**Direct numerical result from MedInjection-FR**: SYN-only 38.23 EM (collapses); NAT+TRAD 49.24 (best). Synthetic alone is the worst single source by far; mixed with native it adds value.

**Implication for us**: Recipe v0.3.1 was 70% distilled (≈SYN) + 20% EN + 10% Roman-NE. **Missing a native Nepali anchor slice.** v0.4 must add 10-25% native data — Anudesh subset of IndicAlign-Instruct or MURI-style reverse-instruction generation on Sangraha Verified.

### Pattern 12: Preference learning (DPO/CPO) underperforms SFT at low-resource scale

| Paper      | Method tried        | vs SFT                                         |
|------------|---------------------|------------------------------------------------|
| Lebanese   | CPO (contrastive PO)| "Consistently below baseline model performance"|
| (others)   | none reported       | No paper in our corpus successfully used preference learning for low-resource adaptation |

**Implication**: Skip DPO/CPO for v0.4. Revisit at v1.0+ when we have human preference data and a stable SFT baseline. Spending iteration cycles on preference methods now is malpractice given the literature.

---

## Open questions

1. ~~**Gemma 3 4B vs Qwen3-8B?**~~ — **Resolved Pass 3**: base is Gemma 4 E4B-IT (user-confirmed). Apache 2 license also resolves the production-deployability concern.
2. **Does STM transfer to language adaptation?** All STM experiments are English domain transfer. Token-level perplexity in a Devanagari sequence will mostly mask Devanagari (high PPL by definition). Might starve the signal. Falsifiable in a small ablation.
3. ~~**What instruction format does Gemma 3 expect for SFT?**~~ — **Resolved Pass 2** (Gemma 3): `<bos><start_of_turn>user/model<end_of_turn>`.
4. ~~**Gemma 4 E4B-IT chat template — same as Gemma 3 or different?**~~ — **Resolved Pass 3 supplement**: DIFFERENT. Gemma 4 uses `<|turn>...<turn|>`, not `<start_of_turn>...<end_of_turn>`. Native `system` role available. Operational gotcha: load via `hf_hub_download` (HF #45205).
5. **PLE LoRA strategy for Gemma 4** — leave PLE projections frozen (default v0.3) or include them in LoRA target_modules? Possibly the largest unknown. Needs empirical test.
6. **MatFormer interaction with fine-tuning** — does LoRA-ing E4B leave the E2B subset usable? Or does it degrade? Test by extracting and evaluating both after training.
7. **`Saugatkafley/alpaca-nepali-sft` data quality** — native Nepali authoring or auto-translated Alpaca? Spot-check 100 random samples.
8. **IndicAlign-Instruct Nepali subset structure** — how many examples per source? Native (Anudesh) Nepali count vs translated/template/synthetic? Audit needed.
9. **DPO / preference tuning** — none of these papers used it for low-resource. Skip for v0.3.
10. **Bytes-per-token on Devanagari Nepali for Gemma 4 E4B** — concrete measurement before training.
11. **Native vs MT controlled experiment** — nobody has run it. 1K vs 1K ablation could be a low-cost publishable contribution.
12. **Distillation prompt design** — Sonnet/Kimi citing chunk URLs without leaking teacher identity. Scrub list: `Claude`, `Sonnet`, `Anthropic`, `Kimi`, `Moonshot`, etc.
13. **Thinking mode on/off for SFT?** — Default v0.3.1: OFF. Reasoner-style fine-tuning is a separate experiment. Question: would Roman-NE handling benefit from explicit reasoning steps? Possible later ablation.
14. **Read MedInjection-FR (2603.06905)** — first published controlled native-vs-MT-vs-synthetic instruction-tuning experiment. Headline: native data anchors learning; combining with MT/synthetic can match or exceed pure-native. Direct evidence for our 70% distilled / 20% English / 10% Roman-NE design.

---

## Concrete plan (v0.4 — adds native Nepali anchor slice; corrects chat template; drops Saugatkafley to lowest priority)

### Recipe v0.4

| Decision           | Value                                                                                          | Source                                  |
|--------------------|------------------------------------------------------------------------------------------------|-----------------------------------------|
| **Base model**     | **Gemma 4 E4B-IT** (Apache 2). 4.5B effective / 8B with embeddings / 128K context              | Gemma 4 HF blog                         |
| MatFormer note     | E2B subset is structurally inside E4B — tune E4B once → deploy on E4B (helpdesk PC) or E2B (Pi 5) | Gemma 4 HF blog                       |
| Method             | QLoRA NF4 + **rsLoRA** (default); ablate **DoRA** as alternative                               | Romanized Nepali, rsLoRA; MedInjection-FR (DoRA) |
| Rank / alpha       | r=32, α=64 (rsLoRA); r=16, α=16 (if DoRA ablation)                                            | Romanized Nepali, GemMaroc; MedInjection-FR |
| **Target modules** | q, k, v, o, gate, up, down (standard) — **PLE left frozen in v0.3**; ablate later              | Romanized Nepali; PLE = open Q          |
| Learning rate      | 1e-4 default; 4e-4 only if data ≤1K. Per Secret Recipe: prefer larger batch + smaller LR if compute allows | Romanized Nepali, GemMaroc, Secret Recipe |
| Optimizer          | AdamW 8-bit                                                                                    | Romanized Nepali                        |
| Warmup             | ~200 steps                                                                                     | Romanized Nepali                        |
| Epochs             | 3 with checkpoint-per-epoch + early stop on val loss                                           | Romanized Nepali, Tsinghua              |
| Batch size         | 16 effective (test 32 if compute allows — Secret Recipe favors larger)                         | Romanized Nepali, Secret Recipe         |
| Sequence length    | 2048                                                                                           | GemMaroc                                |
| Precision          | bf16 forward; NF4 base; 16-bit rsLoRA adapters                                                 | Romanized Nepali, QLoRA                 |
| **Chat template**  | **Gemma 4 format** (`<\|turn>...<turn\|>`, NOT Gemma 3's `<start_of_turn>`). Load via `hf_hub_download("google/gemma-4-E4B-it", "chat_template.jinja")`. **Disable thinking mode** for SFT | HF + Google AI model cards; HF #45205 |
| Roles              | Use `system` (RAG context) + `user` (question) + `model` (grounded answer). Native system role is new in Gemma 4. | Gemma 4 model card |
| Thinking mode      | **Disabled** — `enable_thinking=False`. Composer is grounded, not a reasoner. | Gemma 4 model card |
| Modality freezing  | Freeze vision (~150M) and audio (~300M) encoders; LoRA only the language path | Gemma 4 architecture |
| Data composition   | 80% Nepali / 20% English replay                                                                | GemMaroc, GaMS-3                        |
| Anti-forgetting    | (a) 20% EN replay; (b) GSM8K-en kill switch ≤−2 pp; (c) average top-3 LoRA seeds               | GemMaroc, GaMS-3, Gemma 2 (averaging)   |
| Identity scrub     | Filter `Claude`, `Sonnet`, `Anthropic`, `Kimi`, `Moonshot`                                     | GaMS-3 (Qwen lesson)                    |
| STM                | Skip in v0.3; ablate later if non-target benchmarks regress                                    | STM (open Q for language adaptation)    |

### Eval plan

| Eval                                | Purpose                       | Target                                |
|-------------------------------------|-------------------------------|---------------------------------------|
| Belebele 200 (Nepali)               | reading comprehension         | within −2 pp of base                  |
| FLORES en→ne 100                    | translation regression check  | ≥ baseline                            |
| FLORES ne→en 100                    | translation regression check  | ≥ baseline                            |
| Roman-NE degen 10                   | language-mix bug              | ≤1/10 (currently 3/10 baseline)       |
| **Indic XNLI Nepali**               | reasoning regression sentinel | within −5 pp                          |
| **GSM8K-en**                        | English kill switch           | ≥ −2 pp (else stop)                   |
| **MMLU-en**                         | English knowledge regression  | within −3 pp                          |
| **AIME 2026 sample**                | reasoning ceiling check       | within −3 pp (E4B baseline 42.5%)     |
| **Groundedness (task #32)**         | actual task                   | TBD — needs eval set built first      |
| **IndicGenBench-Nepali**            | open-domain Nepali generation | **available** — `google-research-datasets/indic-gen-bench`; Gemma 3 27B baseline 63.4% overall |
| **LLM-as-judge (Sonnet)**           | groundedness + fluency rubric | per MedInjection-FR — r=0.61 with experts |
| **Position-bias check**             | randomize MCQ answer order    | rankings stable per MedInjection-FR   |

The **groundedness eval set is the gating prerequisite**. Without it we measure regression but not improvement.

### Data plan (v0.4 — native anchor added per MedInjection-FR + Lebanese)

| Slice                       | Source                                                                              | Size goal | Status |
|-----------------------------|--------------------------------------------------------------------------------------|-----------|--------|
| 50% — task tuples (synthetic) | Distilled (Sonnet/Kimi K2.6) `(question, retrieved_chunks, grounded_answer)`        | ~5K       | not yet generated |
| 15% — native Nepali anchor   | IndicAlign-Instruct Anudesh-Nepali subset (crowdsourced native)                     | ~1.5K     | extract from AI4Bharat repo |
| 10% — MURI-style reverse-instruction | Sangraha Verified Nepali → MADLAD/Sonnet → back-translate; output stays native | ~1K | new pipeline; reproducible from MURI |
| 10% — Roman-NE handling     | Curated formal Devanagari ↔ Roman-NE pairs                                          | ~1K       | not yet curated |
| 15% — English replay        | TULU subset (reasoning-dense per GemMaroc)                                          | ~1.5K     | available |
| **Total**                   |                                                                                      | **~10K**  | |

**Why this composition**:
- MedInjection-FR: NAT-only 47.13 → NAT+TRAD 49.24 with half the native data. Mixtures beat singles.
- SYN-only 38.23 EM (collapse). 50% distilled needs the 25% native (15% Anudesh + 10% MURI) to anchor.
- Lebanese: 3K native > 140K translated. Even a small native slice changes outcomes.
- Borsch: 7% native → 31-pt fluency loss. We're now at 25% native — well above the failure-mode threshold.

**Demoted candidates**:
- `Saugatkafley/alpaca-nepali-sft` (52K) — **MT Alpaca, no Nepal-domain knowledge** (audit confirmed Pass 4). Use only as ≤10% language-fluency filler if data short, otherwise skip.
- Navarasa 2.0's `nepali_alpaca_yahma_cleaned_filtered` — translated Alpaca only; lowest priority.

**Pretraining (only if SFT alone fails the regression budget)**:
- AI4Bharat **Sangraha Verified Nepali = 1.8B native-quality tokens.** Reserved.

**English replay**: TULU subset, ~1.5K examples (15% of mix).

### Phase order (executable)

1. **Verify Gemma 4 E4B-IT chat template loading** — `hf_hub_download` jinja, set `tokenizer.chat_template`, run a smoke test.
2. **Decide PLE LoRA strategy** — read Gemma 4 model implementation; small LoRA-with-PLE vs LoRA-without-PLE ablation on a 1K subset.
3. **Build groundedness eval set (task #32)** — 100 native Nepali items. Gating per CLAUDE.md.
4. Tokenizer-fertility on Gemma 4 E4B vs Nepali Wikipedia paragraph (bytes/token).
5. **Extract IndicAlign-Instruct Anudesh-Nepali subset** (~1.5K target, native crowdsourced).
6. **Build MURI reverse-instruction pipeline** for Sangraha Verified Nepali → ~1K native-output instruction pairs.
7. Distillation pipeline → ~5K grounded synthetic tuples; identity-scrub; format into Gemma 4 chat template (with `system` role for retrieved context).
8. Curate ~1K Devanagari ↔ Roman-NE pairs.
9. Train v0.4 LoRA (rsLoRA r=32) on Gemma 4 E4B-IT. Save checkpoints per epoch.
10. Eval against the suite. Apply kill switches. Use position-bias-randomized MCQ runs per MedInjection-FR.
11. Average top-3 LoRA seeds (Gemma 2 model-averaging trick).
12. **Ablation 1**: rsLoRA r=32 vs DoRA r=16 (MedInjection-FR setup).
13. **Ablation 2 (only if v0.4 misses groundedness)**: PLE LoRA on/off, STM on/off, English share 15% vs 25%.
14. If 4B fails regression budget → escalate to 26B-A4B, or CPT-then-SFT with Sangraha Verified.

### Still open before training

- [x] ~~Audit `Saugatkafley/alpaca-nepali-sft`~~ — done Pass 4 (MT Alpaca, demoted)
- [ ] **Build groundedness eval set (task #32)** — biggest blocker.
- [ ] **Verify Gemma 4 E4B-IT chat template loading** — `hf_hub_download` + smoke test (~10 min).
- [ ] **Decide PLE LoRA strategy** — read Gemma 4 implementation or run 1K ablation.
- [ ] Tokenizer-fertility measurement on Gemma 4 E4B for Devanagari.
- [ ] Extract IndicAlign-Instruct Anudesh-Nepali subset from AI4Bharat repo (~1.5K target).
- [ ] Build MURI-style reverse-instruction pipeline for Sangraha Verified Nepali (~1K native pairs).
- [ ] Distillation prompt template + Sonnet vs Kimi cost/quality decision (~5K target).
- [ ] Native-vs-MT ablation: include in v0.4 (we have the data infrastructure now), or defer to v0.5.

---

## Research stopping point (Pass 5 close)

After 5 passes and ~17 papers/sources, the four-part stop condition is satisfied:

| Question                  | Resolved?              | Anchored on                                              |
|---------------------------|------------------------|----------------------------------------------------------|
| (a) data mix              | **Yes**                | MedInjection-FR + GaMS-3 + Lebanese + GemMaroc convergence |
| (b) anti-forgetting       | **Yes (defaults set)** | 20% English replay + GSM8K kill switch + LoRA seed averaging; STM left as falsifiable open question |
| (c) PEFT vs full FT       | **Yes**                | QLoRA NF4 + rsLoRA r=32; full FT requires GaMS-3-scale compute |
| (d) eval target           | **Mostly**             | Regression budgets set; groundedness target TBD pending task #32 |

**Recipe v0.4 is executable** subject to four pre-flight checks that are *operations*, not research:

1. Build groundedness eval set (task #32) — biggest blocker.
2. Verify Gemma 4 chat template loads via `hf_hub_download` + smoke test.
3. Decide PLE LoRA strategy (read Gemma 4 implementation or run 1K ablation).
4. Tokenizer-fertility check on Gemma 4 E4B for Nepali Devanagari.

Then the data pipeline (extract Anudesh-Nepali, build MURI reverse-instruction pipeline, distill ~5K grounded tuples) and training (LoRA on Gemma 4 E4B-IT).

**Reading more papers at this point has diminishing returns.** The remaining queue items (DoRA full read, PiSSA, MURI Indic GEC, etc.) would tighten v0.4 → v0.5 at the margins, not change architecture. Better to spend the next iteration on the operational pre-flight + eval-set construction, then read paper-by-paper if v0.4 results disagree with the literature.

---

## Appendix: still in queue (deferred — read only if v0.4 fails)

- **2603.06905** — MedInjection-FR: controlled native vs synthetic vs translated instruction tuning (NEW Pass 3) ⭐
- **2409.12958** — MURI: Reverse Instructions for low-resource instruction tuning (NEW Pass 3)
- **2505.00114** — Lebanese low-resource fine-tuning (NEW Pass 3)
- vLLM Gemma 4 recipe (github.com/vllm-project/recipes/blob/main/Google/Gemma4.md)
- **2305.14314** — QLoRA (Dettmers et al) — abstract done; full paper desirable
- **2412.13337** — Secret Recipe (Pareja et al) — abstract done; full paper desirable
- **2106.09685** — LoRA original (Hu et al) — abstract only so far
- **2402.09353** — DoRA — abstract done
- **2512.00219** — Minimal-Edit Instruction Tuning for Low-Resource Indic GEC (peripheral)
- PiSSA (newer PEFT variant — find arxiv id)
