# Reliability Record: Command Lowering Gap

- **Code**: `DrawSession::lower_graph_command()`, `DrawSession::lower_session_command()`,
  `gla_ir::GraphCommand`, `gla_ir::SessionCommand`
- **Classification**: Unverified — undecided design
- **Why**: Human confirms command lowering semantics are undecided. Currently all
  reads lower to `Derive::Copy` regardless of command identity. `RenderTo` and
  `Clear` exist as `gla_image_command::Derive` variants but are not generated
  during session lowering. `GraphCommand`/`SessionCommand` hold only reads,
  no operation semantics (blend mode, merge, blur, etc.).
- **Impact on analysis**: Do not assume Copy-only lowering is the final design.
  Future command types may need operation identity in `GraphCommand`/`SessionCommand`.
