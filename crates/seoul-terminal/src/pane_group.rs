use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::pane::Pane;

/// Axis for splitting panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Recursive tree of panes supporting arbitrary splits.
/// Inspired by Zed's `PaneGroup` in `crates/workspace/src/pane_group.rs`.
pub enum PaneGroup {
    /// A single pane (leaf node).
    Single(Entity<Pane>),
    /// A split containing two or more children along an axis.
    Split {
        axis: Axis,
        children: Vec<PaneGroup>,
        /// Proportional sizes (0.0..1.0), must sum to 1.0.
        ratios: Vec<f32>,
    },
}

impl PaneGroup {
    /// Create a group containing a single pane.
    pub fn single(pane: Entity<Pane>) -> Self {
        PaneGroup::Single(pane)
    }

    /// Split the pane that matches `target` along `axis`, creating a new pane via `create_pane`.
    /// Returns the newly created pane, or None if target was not found.
    pub fn split(&mut self, target: &Entity<Pane>, axis: Axis, new_pane: Entity<Pane>) -> bool {
        match self {
            PaneGroup::Single(pane) => {
                if pane == target {
                    let old_pane = pane.clone();
                    *self = PaneGroup::Split {
                        axis,
                        children: vec![PaneGroup::Single(old_pane), PaneGroup::Single(new_pane)],
                        ratios: vec![0.5, 0.5],
                    };
                    true
                } else {
                    false
                }
            }
            PaneGroup::Split { children, .. } => {
                for child in children.iter_mut() {
                    if child.split(target, axis, new_pane.clone()) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Remove a pane from the tree. Returns true if found and removed.
    /// If a Split is left with one child, it collapses to that child.
    pub fn remove(&mut self, target: &Entity<Pane>) -> bool {
        match self {
            PaneGroup::Single(pane) => pane == target,
            PaneGroup::Split {
                children, ratios, ..
            } => {
                if let Some(idx) = children
                    .iter()
                    .position(|c| matches!(c, PaneGroup::Single(p) if p == target))
                {
                    children.remove(idx);
                    ratios.remove(idx);
                    // Normalize ratios
                    let sum: f32 = ratios.iter().sum();
                    if sum > 0.0 {
                        for r in ratios.iter_mut() {
                            *r /= sum;
                        }
                    }
                    // Collapse if only one child remains
                    if children.len() == 1 {
                        let only = children.remove(0);
                        *self = only;
                    }
                    return true;
                }
                // Recurse into children
                for child in children.iter_mut() {
                    if child.remove(target) {
                        return true;
                    }
                }
                // Check if any child collapsed to empty after recursive removal
                false
            }
        }
    }

    /// Collect all pane entities in the tree (left to right / top to bottom).
    pub fn panes(&self) -> Vec<Entity<Pane>> {
        match self {
            PaneGroup::Single(pane) => vec![pane.clone()],
            PaneGroup::Split { children, .. } => children.iter().flat_map(|c| c.panes()).collect(),
        }
    }

    /// Find the pane that contains a given pane entity.
    #[allow(dead_code)]
    pub fn contains(&self, target: &Entity<Pane>) -> bool {
        match self {
            PaneGroup::Single(pane) => pane == target,
            PaneGroup::Split { children, .. } => children.iter().any(|c| c.contains(target)),
        }
    }

    /// Render the pane group recursively.
    pub fn render(&self, _cx: &App) -> AnyElement {
        match self {
            PaneGroup::Single(pane) => pane.clone().into_any_element(),
            PaneGroup::Split {
                axis,
                children,
                ratios,
            } => {
                let is_horizontal = *axis == Axis::Horizontal;
                let mut container = div().id("pane-group").size_full().flex();

                if is_horizontal {
                    container = container.flex_row();
                } else {
                    container = container.flex_col();
                }

                for (child, &ratio) in children.iter().zip(ratios.iter()) {
                    let child_el = child.render(_cx);
                    // Use flex_basis with ratio percentage
                    let pct = ratio * 100.0;
                    container = container.child(
                        div()
                            .id(ElementId::Name(format!("split-{pct:.0}").into()))
                            .flex_basis(relative(ratio))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .h_full()
                            .when(!is_horizontal, |el| el.w_full())
                            .child(child_el),
                    );
                }

                container.into_any_element()
            }
        }
    }
}
