# Rust coding guidelines

* Do not duplicate any logic, try to reuse by importing and using existing implementations.
* Prioritize code correctness and clarity. Speed and efficiency are secondary priorities unless otherwise specified.
* Do not write organizational or comments that summarize the code. Comments should only be written in order to explain "why" the code is written in some way in the case there is a reason that is tricky / non-obvious.
* Avoid using functions that panic like `unwrap()`, instead use mechanisms like `?` to propagate errors.
* Be careful with operations like indexing which may panic if the indexes are out of bounds.
* Never silently discard errors with `let _ =` on fallible operations. Always handle errors appropriately:
  - Propagate errors with `?` when the calling function should handle them
  - Use `.log_err()` or similar when you need to ignore errors but want visibility
  - Use explicit error handling with `match` or `if let Err(...)` when you need custom logic
  - Example: avoid `let _ = client.request(...).await?;` - use `client.request(...).await?;` instead
* Avoid creative additions unless explicitly requested
* Use variable shadowing to scope clones in async contexts for clarity, minimizing the lifetime of borrowed references.
  Example:
  ```rust
  executor.spawn({
      let task_ran = task_ran.clone();
      async move {
          *task_ran.borrow_mut() = true;
      }
  });
  ```
* treat keys and ids seriously, never create structs with magic keys or ids, the should only be provided by somewhere with enough context to know what they mean.
* prefer index mapping over key lookup and Hash maps for performance.

# Interface and crate boundary guidelines

* **Stop and discuss** anytime you find there are multiple ways to implement a feature or design choices should be made. Never fall back to a compile-only standard.
* Do not introduce `*Plan`, `*Request`, or similar intermediate data structures unless they are true domain objects with independent value. Do not split one linear feature into multiple staged APIs just to pass private structs around.
* Crate-to-crate communication should prefer direct use of the callee crate's public atomic APIs. Do not create private orchestration structs as an ad hoc protocol between crates.
* Keep public APIs at the level of one atomic responsibility. If a function is just a thin wrapper around another public API plus trivial field shuffling, it should usually not exist.
* Do not confuse logical images with texture images. Renderer-facing APIs should describe GPU/atlas resources; document-facing APIs should describe document state and serialized assets.
* All GPU operations must be initiated by `renderer`. Other crates may orchestrate GPU work, but they must do so by calling renderer public APIs rather than reimplementing GPU logic.
* `atlas` owns translation from `TileKey` to atlas coordinates or addresses. Other crates must not duplicate that mapping logic.
* `gla_document` owns document structure and document serialization format. If another crate needs document bytes or tile asset paths, prefer adding a small public API to `gla_document` instead of re-encoding the format elsewhere.
* `app` is the orchestration layer. It may occupy the whole document during save/export and directly walk document state when needed. Favor simple straight-line orchestration over cached export plans.
* Persistence is allowed to prioritize clarity over throughput. For save/export flows triggered by user actions or shutdown, prefer simpler full-document passes instead of incremental abstraction layers.
