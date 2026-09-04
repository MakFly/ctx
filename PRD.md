# PRD — ctx

**Produit** : runner d'exploration de codebase pour harness d'agents (Claude Code, Codex, OpenCode, Cursor)
**Livrables associés** : index local ultra-rapide + compte rendu durable
**Version** : 0.2
**Statut** : implémentation Rust
**Date** : 2026-09-04

---

## 1. Vision

Un agent de code perd la majorité de son temps à chercher : Glob, Grep, Read, encore Grep. Sur un monorepo, `rg` peut prendre des secondes. Sur n'importe quel repo, 10–20 tool calls plus tard, le contexte est pollué et rien n'est réutilisable à la session suivante.

`ctx` inverse ça :

- Indexer localement la codebase (lexical + symboles + graphe) pour répondre en millisecondes.
- Orchestrer l'exploration via le harness, avec un playbook borné — pas une errance.
- Écrire un compte rendu (`briefing.md` + `briefing.json`) qu'un humain ou le prochain agent peut charger au lieu de ré-explorer.

Ce n'est pas un RAG cloud. Ce n'est pas un wrapper `rg`. C'est un dossier d'opération sur le repo, produit en < 90 s, cité `file:line`.

---

## 2. Problème

| Symptôme | Cause | Coût |
|---|---|---|
| L'agent grep 15 fois pour trouver l'auth | Pas d'index, mauvaise modalité | Tours LLM (secondes) + tokens |
| `rg` à 15 s sur un monorepo | Scan linéaire, pas d'n-grams | L'agent attend, puis spécule |
| Session 2 recommence à zéro | Pas d'artefact durable | Onboard / handoff payés N fois |
| Grep « qui appelle X » est faux | Commentaires, strings, homonymes | Mauvais edit, blast radius raté |
| Résultat = mur de hits | Pas de ranking ni budget tokens | Contexte saturé |

Constat mesuré (Entire.io, 2026) : passer `rg` de 14,7 ms à 1,7 ms ne change presque rien au wall-clock. Le levier, c'est moins de tours + résultats bornés + briefing réutilisable.

---

## 3. Objectifs

### 3.1 Goals

- **G1.** Recherche lexicale / symbole / graphe p50 < 10 ms, p99 < 50 ms (repo warm, ≤ 1M LOC).
- **G2.** Un appel `ctx explore` produit un briefing actionnable en ≤ 90 s (mode harness) ou ≤ 5 s (mode déterministe).
- **G3.** Réduire d'au moins 50 % les tool calls d'exploration vs Grep+Read nu, et d'au moins 5× les tokens de hits bruts.
- **G4.** Le briefing de session N+1 évite de ré-explorer tant que le SHA n'a pas divergé au-delà du seuil.
- **G5.** Intégration native Claude Code + Codex (MCP + skill + CLI), 100 % local, aucun code envoyé hors machine.

### 3.2 Non-goals (v1)

- Wiki narratif type DeepWiki / documentation marketing.
- Embeddings transformer cloud, Turbopuffer, GPU.
- LSP obligatoire au hot path (rust-analyzer, tsserver…).
- 16+ tools MCP.
- Multi-repo org-wide type Sourcegraph.
- Édition / refactor automatique (on explore et on briefe, on n'écrit pas le patch).
- UI web riche (un markdown + JSON suffisent).

---

## 4. Utilisateurs

| Persona | Job to be done |
|---|---|
| Dev qui drop un agent sur un repo inconnu | Comprendre où aller en 10 min |
| Dev qui lance un change | Savoir quels fichiers toucher + quels tests |
| Agent session 2 (Claude Code / Codex) | Charger le briefing au lieu de re-grep |
| Tech lead / reviewer | Blast radius, hotspots, gotchas |

Le client primaire de l'API, c'est l'agent. L'humain lit le markdown.

---

## 5. Surfaces produit

```
ctx                    CLI
ctx mcp                serveur MCP stdio
.ctx/briefing.md       compte rendu humain
.ctx/briefing.json     compte rendu machine
.ctx/map.json          carte déterministe
SKILL.md / AGENTS.md   playbook harness
```

Quatre commandes utilisateur. Pas plus.

| Commande | Rôle |
|---|---|
| `ctx index` | Construit / met à jour l'index |
| `ctx search` / `ctx graph` | Retrieval ad hoc |
| `ctx map` | Carte déterministe, 0 LLM |
| `ctx explore` | Speed-run + briefing |

---

## 6. Architecture

```text
fichiers (.gitignore, skip binaires / >1 Mo)
        |  watch (inotify / FSEvents / notify) + hash XXH3
        v
+------------------------------------------------------------+
| L0  n-grams / trigrams mmap     lexical, regex, id exact    |
| L1  symboles tree-sitter        def, sig, span, kind        |
| L2  graphe imports / calls      callers, path, impact       |
| L3  embeddings statiques opt.   queries conceptuelles       |
+------------------------------------------------------------+
        |
        v
   ranking definition-first + budget tokens + coverage
        |
        +- MCP / CLI  (hits bornes)
        +- explore playbook -> briefing.md + briefing.json
```

### 6.1 Principes

- **MVP** : binaire Rust natif `ctx`, un point d'entrée console, SQLite FTS5 embarqué.
- **V1.1** : n-grams mmap + daemon dans le même moteur Rust, même CLI / mêmes schémas. L'API ne change pas.
- **Overlay dirty files** : index piné sur HEAD + couche uncommitted. Un agent qui vient d'écrire un fichier doit le retrouver.
- **Incremental** : 1 fichier modifié = reparse cible < 100 ms.
- **Fallback `rg`** en subprocess si l'index ne peut pas garantir un regex. Le signaler dans `coverage`.
- **Tree-sitter** sur le hot path. LSP optionnel comme enrichissement explicite, jamais comme search.
- **L3** (embeddings locaux type Model2Vec / GloVe 50d) hors MVP. Le playbook + BM25 + symboles suffisent pour v1.

### 6.2 Stockage

Répertoire `.ctx/` à la racine du repo (gitignore-able, commitable au choix) :

```
.ctx/
  index/          shards mmap + meta SQLite
  map.json
  briefing.md
  briefing.json
  log.jsonl       traces explore (debug)
```

SQLite WAL + FTS5 pour métadonnées, symboles, arêtes et lexical MVP.
V1.1 : fichiers mmap pour postings n-grams (lookup table triée + postings), modèle Cursor Instant Grep / Zoekt simplifié — sans changer l'API.

---

## 7. Contrats API

### 7.1 CLI

```
ctx init
ctx index [--watch]
ctx status
ctx search  <q> [--mode auto|text|symbol|ast] [--path P] [--limit 20] [--budget-tokens 1500]
ctx graph   --op def|refs|callers|callees|path|impact --symbol S [--depth 2]
ctx map     [--out .ctx/map.json]
ctx explore [--intent onboard|change|handoff|impact]
            [--focus "auth"]
            [--harness none|claude|codex]
            [--budget 90s]
            [--out .ctx/]
ctx mcp
ctx install --target claude|codex|opencode
```

Toute commande sauf `init` / `install` / `mcp` accepte `--json`.

### 7.2 Tools MCP (4, pas 16)

```
ctx_file    (q, limit=20)
ctx_search  (q, mode=auto|text|symbol|ast, path?, glob?, limit=20, budget_tokens=1500)
ctx_graph   (op=def|refs|callers|callees|path|impact, symbol, depth=1)
ctx_pack    (q, budget_tokens=2000, intent=explore|edit|review)
```

`ctx_explore` n'est pas un tool MCP. C'est une commande CLI (et un skill) pour ne pas laisser l'agent relancer une exploration dans l'exploration.

### 7.3 Envelope unique (toutes les tools)

```json
{
  "hits": [
    {
      "path": "src/auth/session.ts",
      "start": 42,
      "end": 88,
      "symbol": "AuthService.login",
      "kind": "def",
      "sig": "(req: LoginReq): Promise<Session>",
      "snippet": "...",
      "score": 0.91,
      "why": "definition + 12 callers"
    }
  ],
  "tokens": 840,
  "freshness_ms": 12,
  "coverage": "complete",
  "hint": null
}
```

- `coverage` ∈ `complete` | `partial` | `text_only`
- `kind` ∈ `def` | `ref` | `call` | `test` | `doc` | `config`
- `hint` non-null si l'agent doit vérifier (graphe partiel, regex fallback, index stale).

C'est le différenciateur : l'agent distingue « rien ne référence ça » de « le graphe est partiel ».

### 7.4 Routing `mode=auto`

| Query | Route |
|---|---|
| Identifiant exact (`AuthService`, `process_payment`) | L1 symbole + L2 graphe |
| Regex / texte (`TODO`, `"payment_failed"`) | L0 n-grams → verify |
| Phrase naturelle (« où est le rate limiting ») | BM25 sur symboles + snippets, puis pack |

---

## 8. Compte rendu

### 8.1 Intents

| Intent | Le briefing permet de… |
|---|---|
| `onboard` | Comprendre le système en 10 min |
| `change` | Savoir où toucher et quoi tester avant d'écrire |
| `handoff` | Lancer un 2ᵉ agent / une autre session sans ré-explorer |
| `impact` | Voir le blast radius d'un symbole ou d'un diff |

Template unique, sections pondérées selon l'intent (voir §8.3).

### 8.2 Fichiers

- `.ctx/briefing.md` — humain + agent
- `.ctx/briefing.json` — machine, source de vérité

Le markdown est un rendu du JSON. Si divergence, le JSON gagne.

### 8.3 Schéma `briefing.json`

```json
{
  "schema": "ctx.briefing.v1",
  "repo": "checkout-api",
  "sha": "a1b2c3d",
  "dirty": true,
  "intent": "change",
  "focus": "retry paiement",
  "generated_at": "2026-09-04T13:12:00Z",
  "freshness_ms": 8000,
  "harness": "claude",
  "identity": {
    "one_liner": "Service de paiement, entrée apps/api/src/server.ts",
    "stack": ["ts", "fastify", "prisma", "stripe"]
  },
  "map": {
    "packages": [
      {"path": "apps/api", "role": "HTTP + workers"},
      {"path": "packages/domain", "role": "règles métier"}
    ],
    "entrypoints": ["apps/api/src/server.ts"],
    "router": [
      {"intent": "retry paiement", "hit": "src/payments/retry.ts:41"}
    ]
  },
  "flows": [
    {
      "name": "checkout",
      "steps": ["HTTP /v1/checkout", "PaymentService.charge", "StripeAdapter", "Outbox"]
    }
  ],
  "contracts": {
    "http": ["POST /v1/checkout"],
    "events": ["payment.succeeded"],
    "tables": ["payments"]
  },
  "hotspots": [
    {
      "path": "src/payments/PaymentService.ts",
      "why": "900 lignes, 14 callers",
      "risk": "high"
    }
  ],
  "change_plan": {
    "touch": ["src/payments/retry.ts", "src/workers/retry.ts", "src/payments/payments.test.ts"],
    "avoid": ["src/payments/StripeAdapter.ts"],
    "tests": ["pnpm test payments"],
    "blast_radius_files": 6,
    "depth": 2
  },
  "hits": [
    {
      "path": "src/payments/retry.ts",
      "start": 41,
      "end": 88,
      "symbol": "RetryQueue.enqueue",
      "kind": "def",
      "why": "cœur du focus"
    }
  ],
  "coverage": "partial",
  "hint": "graphe TS ok, bindings rust non parsés"
}
```

### 8.4 Rendu markdown (ordre des sections)

Toujours dans cet ordre. Une section vide est omise, jamais remplacée par de la prose inventée.

```
# Briefing <repo> @ <sha>   freshness: …   intent: …

## À quoi ça sert
## Carte (où aller)
## Flux (max 7)
## Contrats
## Hotspots
## Pour changer X          <- obligatoire si intent=change|impact
## Preuves                 <- file:line cités, jamais d'affirmation sans hit
```

**Règle d'or** : toute affirmation structurelle cite au moins un `path:line`. Sinon elle n'entre pas dans le briefing.

### 8.5 Fraîcheur

- `sha` = `git rev-parse HEAD`
- `dirty` = working tree non clean
- Si `briefing.json` existe et `sha` identique et `dirty=false` → `ctx explore` no-op (sauf `--force`)
- Si dirty : régénérer uniquement les packs des fichiers dirty + resynthèse des sections touchées

---

## 9. Playbook explore

### 9.1 Mode `--harness none` (déterministe, MVP jour 1)

0 LLM.

1. Walk + ignore.
2. Tree-sitter → symboles + imports.
3. PageRank fichiers (edges = imports / refs de noms).
4. Heuristiques entrypoints (`main`, `server.ts`, `app.py`, `cmd/`, `apps/*/src`).
5. Si `--focus` : BM25 + lookup symbole sur le focus, top hits.
6. Écrire `map.json` + briefing skeleton (identité pauvre, carte riche, flux approximatifs par imports).

Cible : ≤ 5 s sur 50k LOC, ≤ 30 s sur 1M LOC.

### 9.2 Mode `--harness claude|codex` (speed-run)

Le CLI ne lance pas un agent libre. Il :

1. Exécute le mode déterministe (base).
2. Écrit un prompt de synthèse borné (voir `BUILD_PROMPT.md` § prompt explore).
3. Soit invoque le harness en headless (`claude -p` / `codex exec`) avec le skill, soit s'arrête et laisse l'humain lancer `/ctx-explore` dans la session déjà ouverte.
4. Le skill impose : tools autorisés = `ctx_pack`, `ctx_graph`, `Read` des spans cités. Grep large interdit. Cap N packs (défaut 4). Timeout `--budget`.

Le harness ne rédige que les sections qui exigent du jugement (identité, flux nommés, `change_plan`). Il n'a pas le droit d'inventer un fichier absent des hits.

Inner loop cible : 1 map + ≤ 4 packs + 1 synthèse. Pas d'Explore subagent libre à 20 tours.

### 9.3 Skill harness (contrat)

Fichier installé par `ctx install` :

- **Claude Code** : `.claude/skills/ctx-explore/SKILL.md` + snippet `CLAUDE.md`
- **Codex** : `AGENTS.md` + skill équivalent
- **OpenCode** : plugin MCP + instruction

Texte court, impératif, voir le prompt compagnon.

---

## 10. Exigences fonctionnelles

**Index**

- **F1.** Respecte `.gitignore`, `.cursorignore`, `.ctxignore`. Ignore `node_modules`, `dist`, `.git`, vendored, binaires, fichiers > 1 Mo.
- **F2.** Langages MVP : TypeScript/JavaScript, Python, Go, Rust. Autres = lexical only (L0) + hint `coverage=partial`.
- **F3.** Watch optionnel (`ctx index --watch`).
- **F4.** `ctx status` : sha indexé, fichiers, symboles, arêtes, stale yes/no, taille disque.

**Retrieval**

- **F5.** `search` ranked, definition-first, source avant tests/vendor.
- **F6.** Jamais plus que `budget_tokens` de snippets.
- **F7.** `graph def` retourne une définition canonique si unique, sinon les N candidats + `coverage=partial`.
- **F8.** `graph callers|refs` : arêtes tree-sitter, pas un grep déguisé. Faux positifs documentés via `coverage`.
- **F9.** `pack` groupe par fichier, trim les bodies, recommande un ordre de lecture.

**Explore**

- **F10.** 4 intents, 1 schéma.
- **F11.** Briefing toujours lié à un SHA.
- **F12.** Mode `none` fonctionne offline, zéro réseau, zéro clé API.
- **F13.** Mode harness échoue proprement si le binaire harness est absent (fallback `none` + warning).

**Intégration**

- **F14.** `ctx mcp` = stdio JSON-RPC MCP 2024+.
- **F15.** `ctx install --target claude` écrit skill + `claude mcp add`.
- **F16.** Sortie CLI humaine compacte ; `--json` pour agents.

---

## 11. Exigences non fonctionnelles

| Critère | Cible v1 |
|---|---|
| p50 search / def / callers-1hop | < 10 ms warm |
| p99 search repo ≤ 1M LOC | < 50 ms |
| Incremental 1 fichier | < 100 ms |
| Cold `ctx map` 50k LOC | < 5 s |
| `ctx explore --harness none` | < 5 s (50k LOC) |
| `ctx explore --harness claude` | ≤ 90 s budget |
| RAM daemon repo moyen | < 200 Mo |
| Taille index | < 1.5× corpus texte (objectif < 0.5×) |
| Confidentialité | aucun upload, aucun réseau requis |
| Déterminisme mode `none` | même SHA → même `map.json` |

---

## 12. Phases

### MVP (2 semaines) — shippable

- Binaire `ctx` : `init`, `index`, `status`, `search` (FTS5/BM25 + tree-sitter symbols), `graph def|refs|callers`, `map`, `explore --harness none`.
- Envelope JSON unique.
- `.ctx/briefing.md` + `.ctx/briefing.json` (intent `onboard` + `change`).
- MCP stdio : `ctx_search`, `ctx_graph`, `ctx_pack`.
- Skill Claude Code + `ctx install --target claude`.
- Langages : TS/JS, Python, Go, Rust et PHP.
- Bench interne : Recall@5 vs `rg` sur 30 queries du repo cible ; tool-calls / tokens / time-to-first-correct-file.

### V1.1

- Go + Rust parsers.
- `--harness claude|codex` headless.
- Overlay dirty files + `--watch`.
- Intent `handoff` + `impact` (diff HEAD vs base).
- N-grams mmap (remplace ou complète FTS5) pour monorepos.

### V1.2

- L3 embeddings statiques locaux optionnels.
- Inner search loop type Code Finder (`ctx_pack` fait 2–3 hops tout seul).
- Refine LSP optionnel pour refs exactes.
- Cache briefing incrémental par fichier dirty.

---

## 13. Success metrics

Mesurés sur un repo fixe + 20 tâches d'exploration (trouver un flux, préparer un change, onboard).

| Métrique | Baseline (Grep+Read) | Cible MVP |
|---|---|---|
| Time-to-first-correct-file | — | −60 % |
| Tool calls exploration | 12–20 | ≤ 6 |
| Tokens hits bruts | 20k–200k | ≤ 3k |
| Accuracy définition (def vs homonyme) | grep ~60 % | ≥ 85 % |
| Réutilisation briefing session N+1 | 0 % | l'agent ne relance pas explore si SHA identique |

---

## 14. Risques

| Risque | Mitigation |
|---|---|
| Tree-sitter refs trop approximatives | `coverage=partial` + fallback search text ; pas de prétention LSP |
| L'agent ignore le skill et grep | Skill court + hint + installer aussi une règle CLAUDE.md / AGENTS.md |
| Index stale après edit agent | Overlay dirty + hash par fichier ; v1.1 watch |
| Scope creep wiki / embeddings | Non-goals écrits ; L3 hors MVP |
| Monorepo 100k+ files, FTS5 insuffisant | Concevoir le storage pour swap n-grams mmap sans changer l'API |
| Synthèse LLM hallucine des fichiers | Interdit d'écrire un path absent de hits / map |

---

## 15. Décisions tranchées

- Nom produit / CLI : `ctx`. Moteur d'index : `ctxpack` (nom interne).
- 4 tools MCP max.
- Briefing = md + json, json source de vérité.
- Pas d'embeddings dans le MVP.
- LSP optionnels et explicitement activés, jamais dans le hot path.
- Local-first, zéro cloud.
- Explore n'est pas un tool MCP.
- Affirmation sans `path:line` = bug.

---

## 16. Open questions (ne bloquent pas le MVP)

- Intent prioritaire du premier utilisateur : onboard vs change vs handoff ? (template identique, pondération seule).
- Committer `.ctx/` dans git, ou le gitignorer par défaut ? Défaut : gitignore, `--commit-brief` optionnel.
- Langage d'implémentation : Rust, non négociable. N-grams mmap / daemon en v1.1, contrats inchangés.
- Headless harness : `claude -p` est instable selon versions — v1.1, pas MVP.

---

## 17. Annexes

### A. Références de conception

- Cursor Instant Grep : n-grams locaux + overlay dirty files.
- Zoekt : trigrams positionnels, <50 ms sur corpus ~2 GB.
- Aider repo map : tree-sitter + PageRank + fit token budget.
- Codebase-Memory / codebase-index : graphe SQLite + evidence contract.
- Sourcegraph Code Finder : inner search loop, l'agent reçoit des spans pas des dumps.
- Claude Code : agentic search volontairement sans index — on ajoute un index local, on ne remplace pas le harness.

### B. Exemple de session cible

```
$ ctx index
indexed 1842 files, 11.4k symbols, 2.1s

$ ctx explore --intent change --focus "retry paiement" --harness none
wrote .ctx/briefing.md  (840 tokens)

# dans Claude Code
> applique le change décrit dans .ctx/briefing.md
# l'agent lit le briefing, Read 3 spans, edit, tests
```

---

*Fin du PRD.*
