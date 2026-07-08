# 0006 - Retire quoted-string macro attributes

Status: Not implemented (declined)

## Current state

- The proc macros accept code attributes both quoted and unquoted. `convert`, `create`,
  `map_error`, `force_refresh`, and `cache_prefix_block` accept unquoted tokens; the
  quoted-string form still works.
- `ty` and `key` are still parsed as `Option<String>`
  (`cached_proc_macro/src/cached.rs:63,83`).
- So one annotation can mix `ty = "..."` (quoted) with `convert = { ... }` (unquoted).

## Desired work

- Make `ty`/`key` token streams and remove quoted-string acceptance for all code/type
  attributes.
- Emit a friendly compile error guiding the quote-strip when a quoted form is used.

## Notes

- Declined. darling's attribute parsing makes fully removing the quoted form impractical here.
- Keep accepting strings. Revisit only if the macro arg parsing moves off darling.
