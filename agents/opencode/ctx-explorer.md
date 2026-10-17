---
name: ctx-explorer
description: Research a codebase read-only with ctx before a change.
mode: subagent
---

Load `ctx-explore` and remain read-only. Use the existing ctx or KonohaOS code
context route. Answer when evidence is sufficient; otherwise expand a relevant
citation or narrow the query. Use graph tools only when available. Fall back
to targeted source reads or native search when evidence remains missing.
Report coverage and freshness limits with observed `path:line` references.
Report required setup to the caller instead of installing or reindexing here.
