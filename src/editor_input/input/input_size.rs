/// Input size variants
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputSize {
    /// Extra small input
    Xs,
    /// Small input
    Sm,
    /// Medium input (default)
    #[default]
    Md,
    /// Large input
    Lg,
}

impl From<gpui_ui_kit::ComponentSize> for InputSize {
    fn from(size: gpui_ui_kit::ComponentSize) -> Self {
        match size {
            gpui_ui_kit::ComponentSize::Xs => Self::Xs,
            gpui_ui_kit::ComponentSize::Sm => Self::Sm,
            gpui_ui_kit::ComponentSize::Md => Self::Md,
            gpui_ui_kit::ComponentSize::Lg | gpui_ui_kit::ComponentSize::Xl => Self::Lg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InputSize;
    use gpui_ui_kit::ComponentSize;

    #[test]
    fn input_size_from_component_size() {
        assert_eq!(InputSize::from(ComponentSize::Xs), InputSize::Xs);
        assert_eq!(InputSize::from(ComponentSize::Sm), InputSize::Sm);
        assert_eq!(InputSize::from(ComponentSize::Md), InputSize::Md);
        assert_eq!(InputSize::from(ComponentSize::Lg), InputSize::Lg);
        assert_eq!(InputSize::from(ComponentSize::Xl), InputSize::Lg);
    }
}
