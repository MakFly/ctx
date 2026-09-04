---
name: ctx-explore
description: Explore rapidement un dépôt avec le MCP ctx avant toute recherche large ou modification d'une zone inconnue.
---

# ctx-explore

Utilise d'abord les tools du serveur MCP `ctx`. Selon le harness, leur nom peut être
préfixé par le nom du serveur, par exemple `ctx__ctx_pack` ou
`mcp__ctx__ctx_pack`.

1. Si `.ctx/briefing.md` correspond au besoin, lis-le et ne relance pas l'exploration.
2. Sinon, appelle `ctx_pack` avec la question et un budget de 2000 tokens.
3. Pour préciser un symbole, appelle `ctx_graph` avec `def` puis `callers`.
4. Utilise `ctx_search` pour une recherche ponctuelle et `ctx_file` pour localiser un path.
5. Si l'index est absent ou stale, exécute `ctx index .`, puis reprends via MCP.
6. Si un graphe reste `partial`, vérifie `ctx lsp status --json`. Si le LSP est déjà
   disponible, lance `ctx lsp enrich --language <langage> --background`; ne télécharge
   jamais un LSP sans demande explicite de l'utilisateur.
7. Si le MCP n'est pas disponible, utilise les commandes CLI équivalentes avec `--json`.
8. Ne lance aucun Grep/Glob large tant que `coverage` n'est pas `text_only`.
9. Lis uniquement les spans `start`-`end` cités; ne dumpe jamais un fichier entier.
10. Rapporte `coverage` et `hint`, et cite chaque conclusion avec un `path:line` issu de ctx.
11. N'invente aucun fichier absent des hits ou de la carte.
