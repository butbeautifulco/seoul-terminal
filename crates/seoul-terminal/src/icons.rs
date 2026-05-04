use gpui::{AnyElement, App, IntoElement, Pixels, RenderOnce, Window, prelude::*, px, svg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
    Check,
    ChevronDown,
    ChevronRight,
    File,
    FileCode,
    Folder,
    FolderOpen,
    GitBranch,
    GitMerge,
    GitPullRequest,
    Info,
    Minus,
    Plus,
    RefreshCw,
    Settings,
    Terminal,
    X,
    XCircle,
}

impl IconName {
    pub fn path(self) -> &'static str {
        match self {
            Self::Check => "icons/lucide/check.svg",
            Self::ChevronDown => "icons/lucide/chevron-down.svg",
            Self::ChevronRight => "icons/lucide/chevron-right.svg",
            Self::File => "icons/lucide/file.svg",
            Self::FileCode => "icons/lucide/file-code.svg",
            Self::Folder => "icons/lucide/folder.svg",
            Self::FolderOpen => "icons/lucide/folder-open.svg",
            Self::GitBranch => "icons/lucide/git-branch.svg",
            Self::GitMerge => "icons/lucide/git-merge.svg",
            Self::GitPullRequest => "icons/lucide/git-pull-request.svg",
            Self::Info => "icons/lucide/info.svg",
            Self::Minus => "icons/lucide/minus.svg",
            Self::Plus => "icons/lucide/plus.svg",
            Self::RefreshCw => "icons/lucide/refresh-cw.svg",
            Self::Settings => "icons/lucide/settings.svg",
            Self::Terminal => "icons/lucide/terminal.svg",
            Self::X => "icons/lucide/x.svg",
            Self::XCircle => "icons/lucide/x-circle.svg",
        }
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: Pixels,
    color: gpui::Rgba,
}

impl Icon {
    pub fn new(name: IconName, color: gpui::Rgba) -> Self {
        Self {
            name,
            size: px(16.),
            color,
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn into_any_element(self) -> AnyElement {
        gpui::IntoElement::into_any_element(self)
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        svg()
            .path(self.name.path())
            .size(self.size)
            .flex_none()
            .text_color(self.color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_name_resolves_to_lucide_asset_path() {
        assert_eq!(IconName::Terminal.path(), "icons/lucide/terminal.svg");
        assert_eq!(
            IconName::ChevronRight.path(),
            "icons/lucide/chevron-right.svg"
        );
    }
}
