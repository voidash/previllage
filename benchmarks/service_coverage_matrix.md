# SpeakGov Service Coverage Matrix

Snapshot: 2026-05-11, after the k2 E2B/no-adapter RAG restart, MoHA office
directory seeding, priority MoHA crawl, and rerank smoke pass.

This matrix is about product risk, not model loss. A service is "covered" only
when we have the right source classes, retrieval routes to them, and at least
one realistic eval catches regressions.

## Current Corpus Snapshot

From k2 `/Volumes/T9/gemma-god/corpus_v2/index.db`:

| Metric | Count |
|---|---:|
| sources | 1,071 |
| live documents | 46,051 |
| chunks | 272,718 |
| chunks_fts | 272,718 |

Registry shape from `corpora/sources_tiered.jsonl`:

| Source class | Count |
|---|---:|
| local level | 753 |
| federal | 76 |
| province | 67 |
| district administration office | 75 |
| area administration office | 81 |
| border administration office | 7 |
| MoHA subordinate office | 6 |
| Nepali mission abroad | 1 |
| province transport office | 1 |

Important correction: the official MoHA office directory is the right source for
DAO/Area/Border Administration Office discovery:

- `https://www.moha.gov.np/en/offices`
- `https://www.moha.gov.np/en/contact`

The earlier `dao{district}.moha.gov.np` guessing pass found only 11 domains.
MoHA's own pages listed 172 office links. After normalization and registry sync,
the registry has 75 DAO, 81 Area Administration Office, and 7 Border
Administration Office sources. The first priority crawl processed 31 of those
office sources and added 1,023 live documents / 1,540 chunks. A follow-up
targeted crawl added Embassy of Nepal, Doha and the Kaski/Pokhara driving
license office, adding another 100 live documents / 358 chunks.

## Legend

| Status | Meaning |
|---|---|
| green | enough for current smoke/demo, but still needs broader evals |
| yellow | source exists and some retrieval works; coverage/routing incomplete |
| red | high-demand area with no reliable eval or missing source class |

## Matrix

| Area | What People Ask | Primary Source Routing | Current k2 Coverage | Eval Status | Risk | Next Action |
|---|---|---|---|---|---|---|
| Passport | new/renewal, fee, appointment, urgent, abroad | `nepalpassport.gov.np`, then embassy/MoFA for abroad | passport 70 docs / 74 chunks; Embassy Doha 28 docs / 26 chunks | green: renewal + fee pass; Qatar lost-passport v5 pilot passes | other missions still not seeded | parse/crawl full MoFA mission directory |
| PAN / tax | PAN, VAT, tax clearance, business tax | `ird.gov.np`; current notices for fees/deadlines | 250 docs / 489 chunks | green: PAN smoke passes | VAT/tax-clearance not tested | add VAT, tax clearance, office/contact evals |
| Police clearance | police report, character certificate, online report | `nepalpolice.gov.np` CID/clearance pages | 190 docs / 151 chunks | green: police clearance passes | portal steps may shift | add status/reprint/abroad variants |
| Driving license | new license, trial, retrial, category add, visit date | `dotm.gov.np`, `transportmanagement.gov.np`, province offices | `dotm.gov.np`: 36 docs / 43 chunks; Kaski transport office 72 docs / 332 chunks | yellow/green: generic + Pokhara v5 pilot pass | other province offices still thin | seed/crawl remaining province transport offices |
| Company registration | new company, login, name reservation, renewal, share changes | `ocr.gov.np` | 124 docs / 207 chunks | green: registration smoke passes | post-registration flows untested | add name reservation, renewal, share-transfer evals |
| National ID | pre-enrollment, appointment, biometric, NIN | `donidcr.gov.np`, DAO/local enrollment offices | 207 docs / 226 chunks | green: national ID passes | local office routing needs DAO/ward context | add appointment/local-office multi-turn evals |
| Vital registration | birth, death, marriage, migration, divorce, corrections | `donidcr.gov.np`, municipality/ward service pages | DONIDCR 207/226; Jiri 428/1675 | yellow: Jiri birth passes | marriage/death/correction mostly untested | add ward/municipality flows for each event type |
| Citizenship | first-time, duplicate/lost, correction, minor, mother/father cases | DAO, MoHA law/FAQ, municipality/ward recommendation | MoHA 415/3568; 75 DAO + 81 AAO registered; 31 office-source priority crawl; DAO Sankhuwasabha 32 docs / 9 chunks; Khandbari 62 docs / 374 chunks | yellow: ambiguous + multi-turn Sankhuwasabha v5 pilot passes | many DAO sites have thin chunks; citizenship service pages uneven | crawl remaining MoHA office sources and add district-level eval matrix |
| Local municipality services | sifaris, birth cert, contact, mayor/chair, ward office, hours | municipality service pages, contact pages, named staff, tacit interviews | 753 local sources in registry; Jiri deeply crawled | yellow/green for Jiri only | high variance across sites; stale staff/contact pages | create top-20 municipality evals and chunk health report |
| Land / Malpot | land ownership, land tax, land records, mutation, map | `dolma.gov.np`, `molcpa.gov.np`, local land revenue offices | DOLMA 291/1478; MoLCPA 255/240 | red: no smoke | high-demand, document-heavy, likely PDF extraction issues | add land service evals and source checklist |
| Foreign employment | labor permit, FEIMS, insurance, welfare, complaint, death/compensation | `dofe.gov.np`, `feb.gov.np`, FEIMS/foreignjob portals | DOFE 54/68; FEB 382/3732; FEIMS/foreignjob 0/0 | yellow: v5 pilot routes labor permit to DOFE and welfare/death to FEB | FEIMS portal coverage missing; DOFE pages are notice-heavy | seed/crawl FEIMS or official public portal pages and add full answer smoke |
| Immigration / visa | visa, arrival, extension, trekking, NRN | `immigration.gov.np` | 51 docs / 81 chunks | red: no smoke | not central but common for foreigners | add tourist/visa-extension evals |
| Consular / embassy | passport abroad, NOC, attestation, power of attorney, embassy contact | `mofa.gov.np`, consular department, embassy domains | MoFA 272/479; Embassy Doha 28/26 | yellow: consular attestation + Qatar passport pilot pass | full mission directory not crawled; nepalconsular source still thin | crawl full MoFA mission directory and nepalconsular service pages |
| Customs | import/export, duty, returning migrant goods | `customs.gov.np` | 252 docs / 91 chunks | red: no smoke | fee/rule recency matters | add customs duty and contact evals |
| Voter / election | voter registration, transfer, voter list, polling center | `election.gov.np` | 47 docs / 40 chunks | red: no smoke | seasonal/current info | add registration/transfer evals |
| Banking / NRB | forex, remittance, complaints, bank contact | `nrb.org.np` | 490 docs / 14,175 chunks | red: no smoke | large corpus can swamp retrieval | add narrow NRB evals and ranking guards |
| Complaint / oversight | Hello Sarkar, corruption complaint, service grievance | OPMCM/Hello Sarkar, CIAA, agency grievance pages | OPMCM/CIAA in registry; Hello Sarkar source not confirmed | red: no smoke | bot often says "call 1111" too generically | seed Hello Sarkar and grievance evals |

## Eval Expansion Rule

Each service area should get four kinds of evals:

1. Direct answerable question with expected authoritative domain.
2. Ambiguous question that should ask compact follow-up plus give useful contact/source.
3. Partial-answer question where only some requested details are supported.
4. Negative/distractor case where same agency appears but wrong service should not be answered.

The expanded seed set starts in `eval/service_eval_expanded_v5_seed.jsonl`.
