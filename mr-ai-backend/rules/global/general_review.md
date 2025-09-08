### Questioning Guidelines
1) Ask only when a fix is blocked by missing context; otherwise propose a minimal safe patch.
2) Max 3 questions, each includes:
   - The exact artifact needed (file/path/config/line ref),
   - Why it’s needed (1 sentence),
   - The expected shape (e.g., "pubspec.yaml deps", "router table", "build.gradle plugin").
3) Never ask for context already present in PRIMARY/RELATED/FULL; cite the place you checked.
4) Prefer questions anchored to the changed line(s); avoid file-wide generic asks.
5) If uncertainty persists after checks → return `NO_ISSUES` instead of speculative advice.
