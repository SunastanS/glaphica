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

## Debug Performance Tracing

Set `GLAPHICA_DEBUG_PIPELINE_TRACE=1` to enable slow-frame pipeline tracing.

Optional:
- `GLAPHICA_DEBUG_PIPELINE_SLOW_MS=<N>`: slow frame threshold in milliseconds, default `4`.

When a frame exceeds threshold, logs include:
- `input_sample`: input batch drain and timestamp normalization
- `smooth_and_resample`: stroke smoothing + resampling
- `brush_handling`: brush dispatch and GPU command build
- `send_to_app_thread`: command enqueue to engine->main command ring
- `accept_by_app_thread`: command dequeue by main thread
- `submit_to_gpu`: main thread GPU command submission
- `show_on_screen`: present phase

Each log line also reports the `bottleneck` stage of that frame.
