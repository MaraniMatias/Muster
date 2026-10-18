use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutTerminalLayout {
    pub active_tab_id: Option<String>,
    pub tabs: Vec<TerminalLayoutTab>,
    pub session_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalLayoutTab {
    pub id: String,
    pub root: TerminalLayoutNode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TerminalLayoutNode {
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    Split {
        id: String,
        direction: SplitDirection,
        ratio: f64,
        first: Box<TerminalLayoutNode>,
        second: Box<TerminalLayoutNode>,
    },
}

impl CheckoutTerminalLayout {
    pub fn reconcile_sessions(&mut self, checkout_session_ids: &[String]) {
        let valid_ids: HashSet<_> = checkout_session_ids.iter().cloned().collect();
        let mut seen_ids = HashSet::new();
        self.tabs = self
            .tabs
            .drain(..)
            .filter_map(|mut tab| {
                tab.root = prune_node(tab.root, &valid_ids, &mut seen_ids)?;
                Some(tab)
            })
            .collect();

        let mut tab_ids: HashSet<_> = self.tabs.iter().map(|tab| tab.id.clone()).collect();
        for session_id in checkout_session_ids {
            if !seen_ids.insert(session_id.clone()) {
                continue;
            }
            let mut tab_id = format!("layout:{session_id}");
            let mut suffix = 1;
            while tab_ids.contains(&tab_id) {
                tab_id = format!("layout:{session_id}:{suffix}");
                suffix += 1;
            }
            tab_ids.insert(tab_id.clone());
            self.tabs.push(TerminalLayoutTab {
                id: tab_id,
                root: TerminalLayoutNode::Session {
                    session_id: session_id.clone(),
                },
            });
        }

        let mut ordered = HashSet::new();
        self.session_order
            .retain(|id| valid_ids.contains(id) && ordered.insert(id.clone()));
        for session_id in checkout_session_ids {
            if ordered.insert(session_id.clone()) {
                self.session_order.push(session_id.clone());
            }
        }
        if !self
            .active_tab_id
            .as_ref()
            .is_some_and(|active| self.tabs.iter().any(|tab| &tab.id == active))
        {
            self.active_tab_id = self.tabs.first().map(|tab| tab.id.clone());
        }
    }

    pub fn validate(&self, checkout_session_ids: &HashSet<String>) -> Result<(), String> {
        let mut tab_ids = HashSet::new();
        let mut split_ids = HashSet::new();
        let mut layout_session_ids = HashSet::new();

        for tab in &self.tabs {
            if tab.id.trim().is_empty() || !tab_ids.insert(tab.id.as_str()) {
                return Err("terminal layout contains an empty or duplicate tab ID".into());
            }
            collect_node_ids(
                &tab.root,
                0,
                checkout_session_ids,
                &mut layout_session_ids,
                &mut split_ids,
            )?;
        }

        if self.tabs.is_empty() {
            if self.active_tab_id.is_some() {
                return Err("terminal layout selects a tab but has no tabs".into());
            }
        } else if !self
            .active_tab_id
            .as_deref()
            .is_some_and(|active| tab_ids.contains(active))
        {
            return Err("terminal layout active tab is not in the checkout layout".into());
        }

        let mut ordered_session_ids = HashSet::new();
        for session_id in &self.session_order {
            if !ordered_session_ids.insert(session_id.clone()) {
                return Err("terminal layout contains a duplicate session order entry".into());
            }
        }
        if ordered_session_ids != layout_session_ids {
            return Err("terminal layout order must contain each pane session exactly once".into());
        }
        if layout_session_ids != *checkout_session_ids {
            return Err(
                "terminal layout must include every session registered to this checkout".into(),
            );
        }
        Ok(())
    }
}

fn prune_node(
    node: TerminalLayoutNode,
    valid_ids: &HashSet<String>,
    seen_ids: &mut HashSet<String>,
) -> Option<TerminalLayoutNode> {
    match node {
        TerminalLayoutNode::Session { session_id } => (valid_ids.contains(&session_id)
            && seen_ids.insert(session_id.clone()))
        .then_some(TerminalLayoutNode::Session { session_id }),
        TerminalLayoutNode::Split {
            id,
            direction,
            ratio,
            first,
            second,
        } => {
            let first = prune_node(*first, valid_ids, seen_ids);
            let second = prune_node(*second, valid_ids, seen_ids);
            match (first, second) {
                (Some(first), Some(second)) => Some(TerminalLayoutNode::Split {
                    id,
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            }
        }
    }
}

fn collect_node_ids(
    node: &TerminalLayoutNode,
    depth: usize,
    checkout_session_ids: &HashSet<String>,
    layout_session_ids: &mut HashSet<String>,
    split_ids: &mut HashSet<String>,
) -> Result<(), String> {
    if depth > 64 {
        return Err("terminal layout is nested too deeply".into());
    }
    match node {
        TerminalLayoutNode::Session { session_id } => {
            if !checkout_session_ids.contains(session_id) {
                return Err("terminal layout session does not belong to this checkout".into());
            }
            if !layout_session_ids.insert(session_id.clone()) {
                return Err("terminal layout contains a session in more than one pane".into());
            }
        }
        TerminalLayoutNode::Split {
            id,
            ratio,
            first,
            second,
            ..
        } => {
            if id.trim().is_empty() || !split_ids.insert(id.clone()) {
                return Err("terminal layout contains an empty or duplicate split ID".into());
            }
            if !ratio.is_finite() || !(0.15..=0.85).contains(ratio) {
                return Err("terminal split ratio must be between 0.15 and 0.85".into());
            }
            collect_node_ids(
                first,
                depth + 1,
                checkout_session_ids,
                layout_session_ids,
                split_ids,
            )?;
            collect_node_ids(
                second,
                depth + 1,
                checkout_session_ids,
                layout_session_ids,
                split_ids,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{CheckoutTerminalLayout, TerminalLayoutNode, TerminalLayoutTab};

    fn one_session_layout() -> CheckoutTerminalLayout {
        CheckoutTerminalLayout {
            active_tab_id: Some("tab-one".into()),
            tabs: vec![TerminalLayoutTab {
                id: "tab-one".into(),
                root: TerminalLayoutNode::Session {
                    session_id: "session-one".into(),
                },
            }],
            session_order: vec!["session-one".into()],
        }
    }

    #[test]
    fn layout_requires_every_unique_session_to_belong_to_the_checkout() {
        let layout = one_session_layout();
        assert!(layout
            .validate(&HashSet::from(["session-one".to_string()]))
            .is_ok());
        assert!(layout
            .validate(&HashSet::from(["session-other-checkout".to_string()]))
            .is_err());
    }

    #[test]
    fn rejects_invalid_ratios_duplicate_panes_and_incomplete_tab_order() {
        let mut layout = one_session_layout();
        layout.tabs[0].root = TerminalLayoutNode::Split {
            id: "split-one".into(),
            direction: super::SplitDirection::Horizontal,
            ratio: f64::NAN,
            first: Box::new(TerminalLayoutNode::Session {
                session_id: "session-one".into(),
            }),
            second: Box::new(TerminalLayoutNode::Session {
                session_id: "session-one".into(),
            }),
        };
        assert!(layout
            .validate(&HashSet::from(["session-one".to_string()]))
            .is_err());

        layout.tabs[0].root = TerminalLayoutNode::Session {
            session_id: "session-one".into(),
        };
        layout.session_order.clear();
        assert!(layout
            .validate(&HashSet::from(["session-one".to_string()]))
            .is_err());
    }

    #[test]
    fn reconciles_missing_extra_and_duplicate_checkout_sessions() {
        let mut layout = CheckoutTerminalLayout {
            active_tab_id: Some("tab-one".into()),
            tabs: vec![
                TerminalLayoutTab {
                    id: "tab-one".into(),
                    root: TerminalLayoutNode::Session {
                        session_id: "session-one".into(),
                    },
                },
                TerminalLayoutTab {
                    id: "tab-duplicate".into(),
                    root: TerminalLayoutNode::Session {
                        session_id: "session-one".into(),
                    },
                },
                TerminalLayoutTab {
                    id: "tab-stale".into(),
                    root: TerminalLayoutNode::Session {
                        session_id: "session-stale".into(),
                    },
                },
            ],
            session_order: vec![
                "session-one".into(),
                "session-one".into(),
                "session-stale".into(),
            ],
        };

        layout.reconcile_sessions(&["session-one".into(), "session-two".into()]);

        assert_eq!(
            layout
                .tabs
                .iter()
                .map(|tab| match &tab.root {
                    TerminalLayoutNode::Session { session_id } => session_id.as_str(),
                    TerminalLayoutNode::Split { .. } => "split",
                })
                .collect::<Vec<_>>(),
            ["session-one", "session-two"]
        );
        assert_eq!(
            layout.session_order,
            vec!["session-one".to_string(), "session-two".to_string()]
        );
        assert_eq!(layout.active_tab_id.as_deref(), Some("tab-one"));
        assert!(layout
            .validate(&HashSet::from([
                "session-one".to_string(),
                "session-two".to_string()
            ]))
            .is_ok());
    }

    #[test]
    fn serializes_the_webview_layout_shape_with_camel_case_session_ids() {
        let value = serde_json::json!({
            "activeTabId": "tab-one",
            "sessionOrder": ["session-one"],
            "tabs": [{
                "id": "tab-one",
                "root": { "kind": "session", "sessionId": "session-one" }
            }]
        });
        let layout: CheckoutTerminalLayout = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            layout.validate(&HashSet::from(["session-one".to_string()])),
            Ok(())
        );
        assert_eq!(serde_json::to_value(layout).unwrap(), value);
    }
}
