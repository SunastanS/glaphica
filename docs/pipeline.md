## Core Pipeline

```
[cursor case]               glaphica/src/main.rs
      |
      v
[imput sample]              app/src/integration.rs
      |
      v
[engine thread]             app/src/engine_thread.rs
      |
      v
[smooth and resample]       stroke_input/src/smoother.rs
      |
      v
[brush handling]            brushes/src/engine_runtime.rs
      |
      v
[GPU command]               thread_protocol/src/gpu_command.rs
      |
      v
[send to app thread]        threads/src/lib.rs
      |
      v
[accept by app thread]      app/src/integration.rs
      |
      v
[submit to GPU]             gpu_runtime/src/render_executor.rs
      |
      v
[show on the screen]        gpu_runtime/src/surface_runtime.rs
```
