# Running Gemma On Raspberry Pi

This is the short reviewer-facing Raspberry Pi path. Detailed historical notes
remain in:

- `docs/runbooks/PI.md`
- `docs/runbooks/PI_E2B_LLAMA_CPP_RUNBOOK.md`
- `docs/runbooks/PREVILLAGE_PI_EDGE_DATA_CARD.md`

## What The Pi Proves

The Pi is an edge/fallback lane, not the full national RAG server. The full
SpeakGov demo can use an office computer for retrieval, ASR, TTS, WhatsApp, and
kiosk UX. The Pi proves that a small open Gemma E2B-class model can run locally
for intake and short service-navigation composition.

Safe claim:

```text
Gemma E2B Q4 runs locally on Raspberry Pi 5 at roughly 6-8 generated tokens per
second for short service-navigation answers.
```

Avoid claiming that the Pi serves the full RAG stack unless the retrieval DB
and source index are copied and wired there.

## Tested Setup

```text
hardware: Raspberry Pi 5
ram: 7.9 GiB
runtime: llama.cpp llama-server
model: google_gemma-4-E2B-it-Q4_K_M.gguf
model size: about 3.5 GB
server: http://<pi-tailnet-ip>:8081
api: /v1/chat/completions
```

Startup shape:

```text
--ctx-size 2048
--threads 4
--parallel 1
--jinja
--reasoning off
--cache-ram 256
--temp 0.3
--top-p 0.9
```

Measured behavior:

```text
project footprint: about 3.9 GB
runtime RSS: about 5.0 GB
short smoke latency: about 2-3 seconds
generation: roughly 6-8 tokens/second
```

## Deploy

From this repository:

```bash
PI_SSH=cdjk@<pi-tailnet-ip> ./scripts/deploy_pi_llamacpp.sh
```

If password auth is needed:

```bash
PI_PASSWORD='...' PI_SSH=cdjk@<pi-tailnet-ip> ./scripts/deploy_pi_llamacpp.sh
```

The deploy script copies the Pi helper scripts, installs/builds `llama.cpp`,
downloads the GGUF, starts `llama-server`, and runs an OpenAI-compatible smoke
test.

## Start Or Restart

On the Pi:

```bash
bash ~/gemma-god-pi/scripts/pi_llamacpp_install.sh
bash ~/gemma-god-pi/scripts/pi_llamacpp_start.sh
BASE_URL=http://127.0.0.1:8081 bash ~/gemma-god-pi/scripts/pi_llamacpp_smoke.sh
```

From this repo:

```bash
BASE_URL=http://<pi-tailnet-ip>:8081 ./scripts/pi_llamacpp_smoke.sh
BASE_URL=http://<pi-tailnet-ip>:8081 ./scripts/pi_llamacpp_office_demo.sh
```

Health check:

```bash
curl http://<pi-tailnet-ip>:8081/health
```

Expected:

```json
{"status":"ok"}
```

## Demo Framing

Use the Pi clip to say:

```text
The main SpeakGov stack uses the full corpus, retrieval, ASR, TTS, and
WhatsApp bridge. The Raspberry Pi lane proves the local model path: a small
Gemma E2B quant can run beside an office counter for intake, privacy, and
offline fallback.
```
