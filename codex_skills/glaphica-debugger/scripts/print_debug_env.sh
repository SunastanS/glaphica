#!/usr/bin/env bash
set -euo pipefail

cat <<'EOF'
Recommended Glaphica debug environment switches (enable only what you need):

  GLAPHICA_BRUSH_TRACE=1
  GLAPHICA_RENDER_TREE_TRACE=1
  GLAPHICA_RENDER_TREE_INVARIANTS=1
  GLAPHICA_PERF_LOG=1
  GLAPHICA_FRAME_SCHEDULER_TRACE=1

Example (run a single cargo test with logs):

  GLAPHICA_BRUSH_TRACE=1 cargo test -p renderer <test_name> -- --nocapture

Note: some GPU-related tests may require:

  --test-threads=1
EOF

