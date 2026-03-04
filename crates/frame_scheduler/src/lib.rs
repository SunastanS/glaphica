use rtrb::PopError;
use thread_protocol::GpuCmdMsg;

pub struct FrameBudget {
    max_commands: usize,
    consumed: usize,
}

impl FrameBudget {
    pub fn new(max_commands: usize) -> Self {
        Self {
            max_commands,
            consumed: 0,
        }
    }

    pub fn remaining(&self) -> usize {
        self.max_commands.saturating_sub(self.consumed)
    }

    pub fn consume(&mut self, count: usize) -> bool {
        if self.consumed + count <= self.max_commands {
            self.consumed += count;
            true
        } else {
            false
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.consumed >= self.max_commands
    }
}

pub trait FrameHandler<Context> {
    type Error;

    fn handle(&mut self, cmd: &GpuCmdMsg, ctx: &mut Context);
    fn finalize_frame(self, ctx: &mut Context);
}

pub struct FrameScheduler;

impl FrameScheduler {
    pub fn run<H, Ctx>(
        &self,
        budget: &mut FrameBudget,
        receiver: &mut rtrb::Consumer<GpuCmdMsg>,
        handler: &mut H,
        context: &mut Ctx,
        non_gpu_handler: &mut dyn FnMut(&GpuCmdMsg),
    ) -> usize
    where
        H: FrameHandler<Ctx>,
    {
        let mut processed = 0;

        while !budget.is_exhausted() {
            match receiver.pop() {
                Ok(cmd) => {
                    match &cmd {
                        GpuCmdMsg::RenderTreeUpdated(_) | GpuCmdMsg::TileSlotKeyUpdate(_) => {
                            non_gpu_handler(&cmd);
                            processed += 1;
                            budget.consume(1);
                            continue;
                        }
                        _ => {}
                    }

                    handler.handle(&cmd, context);
                    processed += 1;
                    budget.consume(1);
                }
                Err(PopError::Empty) => break,
            }
        }

        processed
    }

    pub fn finalize<H, Ctx>(self, handler: H, context: &mut Ctx)
    where
        H: FrameHandler<Ctx>,
    {
        handler.finalize_frame(context);
    }
}
