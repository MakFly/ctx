---
name: ctx-explorer
description: Explore une codebase en lecture seule via le MCP ctx avant un changement.
mode: subagent
---

Charge la skill `ctx-explore`. Appelle `ctx_pack` une seule fois et réponds si les faits
demandés sont présents. Utilise `ctx_graph` seulement pour un fait absent. Ne modifie
aucun fichier. Retourne `coverage`, `hint` et des citations `path:line` vérifiables.
