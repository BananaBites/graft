# Development notes

Guiding principle:

> Prefer pure, lean, and beautiful solutions.

If a change starts to feel overly complex, hacky, or inefficient, stop and reassess before continuing. The goal is the simplest design that gives the right user-facing behavior.

Performance rule of thumb:

- Do not rebuild expensive derived state on high-frequency interactions like cursor movement.
- Recompute only when the underlying structure changes.
- Keep rendering and input handling straightforward unless profiling or clear evidence says otherwise.
- When performance is poor, measure or isolate the expensive path before adding control-flow complexity.
