use anyhow::{Context, Result};
use cairo::{Context as CairoContext, Format, ImageSurface};

use crate::hardware::LogicalFrame;

pub(crate) struct FrameCanvas {
    surface: ImageSurface,
    context: CairoContext,
}

impl FrameCanvas {
    pub(crate) fn new() -> Result<Self> {
        let surface = ImageSurface::create(
            Format::ARgb32,
            sliver_core::STRIP_W as i32,
            sliver_core::STRIP_H as i32,
        )
        .context("creating frame surface")?;
        let context = CairoContext::new(&surface).context("creating frame drawing context")?;
        context.set_operator(cairo::Operator::Source);
        context.set_source_rgb(0.0, 0.0, 0.0);
        context.paint().context("clearing frame")?;
        context.set_operator(cairo::Operator::Over);
        Ok(Self { surface, context })
    }

    pub(crate) fn context(&self) -> &CairoContext {
        &self.context
    }

    pub(crate) fn finish(self) -> Result<LogicalFrame> {
        self.surface.flush();
        LogicalFrame::from_surface(&self.surface)
    }
}
