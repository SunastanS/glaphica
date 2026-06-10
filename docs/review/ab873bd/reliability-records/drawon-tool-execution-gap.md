# Reliability Record: DrawOn Tool Execution Gap

- **Code**: `DrawSession::draw_dab()`, `DrawOnInput._tool`, `DrawOnInput._tool_params`
- **Classification**: Unverified — undecided design
- **Why**: Human confirms the final tool execution model is undecided. Current
  implementation only clears destination tiles and marks dirty. Tool/tool_params
  are stored but unused.
- **Impact on analysis**: Do not use the current clear-only behavior as evidence
  of intended final design. Future tests and modifications should wait for the
  tool execution model to be decided.
