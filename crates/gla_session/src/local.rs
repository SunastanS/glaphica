use gla_image::GlaImageLayout;
use gla_ir::SessionCommand;

#[derive(Clone, Debug, PartialEq)]
pub enum LocalImageDeclaration {
    Primitive {
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
    },
    Derived {
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
        command: SessionCommand,
    },
}

impl LocalImageDeclaration {
    pub fn primitive(format: gla_color::GlaFormat, layout: GlaImageLayout) -> Self {
        Self::Primitive { format, layout }
    }

    pub fn derived(
        format: gla_color::GlaFormat,
        layout: GlaImageLayout,
        command: SessionCommand,
    ) -> Self {
        Self::Derived {
            format,
            layout,
            command,
        }
    }

    pub fn format(&self) -> gla_color::GlaFormat {
        match self {
            Self::Primitive { format, .. } | Self::Derived { format, .. } => *format,
        }
    }

    pub fn layout(&self) -> GlaImageLayout {
        match self {
            Self::Primitive { layout, .. } | Self::Derived { layout, .. } => *layout,
        }
    }
}
