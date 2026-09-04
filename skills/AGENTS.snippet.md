## Exploration avec ctx
Le serveur MCP `ctx` et la skill `ctx-explore` sont la voie d'exploration par défaut.
Lis `.ctx/briefing.md` s'il répond déjà au besoin.
Préfère le tool MCP `ctx_pack` à Grep pour découvrir un sous-système.
Utilise `ctx_graph` avec `def` puis `callers` pour suivre un symbole.
Utilise `ctx_search` pour une recherche ciblée et `ctx_file` pour un path.
Si le MCP est indisponible, utilise les commandes `ctx ... --json` équivalentes.
Lis ensuite uniquement les spans cités par ctx.
Ne déduis jamais l'existence d'un fichier absent des résultats.
Chaque conclusion structurelle doit citer un `path:line`.
