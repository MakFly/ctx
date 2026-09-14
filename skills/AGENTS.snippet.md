## Exploration avec ctx
Le serveur MCP `ctx` et la skill `ctx-explore` sont la voie d'exploration par défaut.
Lis `.ctx/briefing.md` s'il répond déjà au besoin.
Préfère `ctx_pack` à Grep pour découvrir un sous-système.
Réponds si le pack contient les faits demandés; si un fait manque, exécute le hint (un autre `ctx_pack`).
N'invente pas de Grep tant que `coverage` n'est pas `text_only`.
Utilise `ctx_search` pour une recherche ciblée et `ctx_file` pour un path.
Si le MCP est indisponible, utilise les commandes `ctx ... --json` équivalentes.
Lis ensuite uniquement les spans cités par ctx.
Ne déduis jamais l'existence d'un fichier absent des résultats.
Chaque conclusion structurelle doit citer un `path:line`.
