# Rust coding guidelines

* Keep diff relatively small (~100 lines) in a roll for reviewing convience.
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

# Interaction guidelines

* No need to point to specific line number in final report, user can track all you edit automatically.
* Be cautious when debugging, unless there are appearant logic or santax errors, agents should use logs or tests to first locate and reappear the error before fixing it.
* For `wgpu` / atlas render-pass changes, check `crates/gpu_runtime/wgpu.md` first. In particular, texture read views must be narrowed to the exact sampled layer(s) to avoid false `RESOURCE` + `COLOR_TARGET` conflicts.

# Project Structure

```
glaphica
├── AGENTS.md
├── Cargo.toml
├── crates
│   ├── app
│   ├── atlas
│   ├── brushes
│   ├── document
│   ├── fram_scheduler
│   ├── glaphica         // entrance of app
│   ├── glaphica_core    // sharing types
│   ├── gpu_runtime      // a thin runtime in app thread to submit gpu command
│   ├── images
│   ├── stroke_input
│   ├── thread_protocol
│   └── threads          // thread model
└── README.md
```
