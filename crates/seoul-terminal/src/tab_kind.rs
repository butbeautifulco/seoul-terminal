use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabKind {
    Terminal,
    Editor,
    Settings,
    Diff,
}

impl TabKind {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            TabKind::Terminal => "terminal",
            TabKind::Editor => "editor",
            TabKind::Settings => "settings",
            TabKind::Diff => "diff",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_variants_round_trip_str() {
        for k in [
            TabKind::Terminal,
            TabKind::Editor,
            TabKind::Settings,
            TabKind::Diff,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: TabKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
            assert_eq!(json.trim_matches('"'), k.as_str());
        }
    }
}
