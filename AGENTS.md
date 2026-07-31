# Project Status

Nole has not been released. Prefer the best current API and data-model design over backward compatibility. Do not add compatibility layers, deprecated aliases, schema fallbacks, or migrations for interfaces that have only existed during development unless the user explicitly requests them.

When writing or modifying Rust code, keep the non-test code in every .rs file under 2000 lines. Necessary inline test code does not count toward this limit, including `#[cfg(test)]` modules, test functions, and test-only helpers that are appropriately colocated with the implementation. If the non-test code approaches this limit, proactively split functionality into submodules (mod) to keep files short and single-purpose.

For selectable list UIs, always reserve a blank row above the first item and draw the shared vertical selection indicator across the complete selected area. Cover both invariants with renderer tests.
