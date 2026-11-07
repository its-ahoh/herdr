//! Client-owned pinned workspaces.
//!
//! Pinning is a sidebar presentation preference: the client remembers which
//! workspace ids are pinned per endpoint and keeps them above unpinned
//! workspaces by issuing the public `workspace.move` / `workspace.move_block`
//! requests. Server order stays authoritative, so pinned workspaces keep their
//! relative order.

use super::*;

impl ClientShellState {
    pub(super) fn pinned_workspaces_for_endpoint(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> Option<&HashSet<String>> {
        if endpoint_id.is_local() {
            Some(&self.pinned_workspaces)
        } else {
            self.remote_pinned_workspaces.get(endpoint_id)
        }
    }

    pub(super) fn workspace_is_pinned(
        &self,
        endpoint_id: &ClientEndpointId,
        workspace_id: &str,
    ) -> bool {
        self.pinned_workspaces_for_endpoint(endpoint_id)
            .is_some_and(|pinned| pinned.contains(workspace_id))
    }

    fn pinned_workspaces_for_endpoint_mut(
        &mut self,
        endpoint_id: &ClientEndpointId,
    ) -> &mut HashSet<String> {
        if endpoint_id.is_local() {
            &mut self.pinned_workspaces
        } else {
            self.remote_pinned_workspaces
                .entry(endpoint_id.clone())
                .or_default()
        }
    }

    /// The first root workspace (not a linked worktree) below the pinned block,
    /// ignoring `excluding`. `None` means the pinned block reaches the end.
    fn first_unpinned_root_workspace(&self, excluding: &str) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.workspace_id != excluding)
            .filter(|workspace| {
                !workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .find(|workspace| {
                !self.workspace_is_pinned(&self.active_endpoint_id, &workspace.workspace_id)
            })
            .map(|workspace| workspace.workspace_id.clone())
    }

    /// Pins or unpins a workspace on the active endpoint and moves it to the
    /// boundary between pinned and unpinned workspaces: the bottom of the pinned
    /// block when pinning, the top of the unpinned workspaces when unpinning.
    pub(super) fn toggle_workspace_pin(
        &mut self,
        workspace_id: String,
        outcome: &mut ClientShellInput,
    ) {
        let endpoint_id = self.active_endpoint_id.clone();
        let pinned = self.pinned_workspaces_for_endpoint_mut(&endpoint_id);
        if !pinned.remove(&workspace_id) {
            pinned.insert(workspace_id.clone());
        }
        self.pinned_repaired_orders.clear();
        self.prune_pinned_workspaces();
        let before = self.first_unpinned_root_workspace(&workspace_id);
        if let Some(method) = self.workspace_move_method(&workspace_id, before.as_deref()) {
            self.push_endpoint_method(method, outcome);
            // This move already restores the pinned block; snapshots that still
            // show the old order must not send a second repair for it.
            let order = self.root_workspace_order();
            self.pinned_repaired_orders.insert(order);
        }
        self.persist_chrome_preferences(outcome);
    }

    /// Runs whenever the active endpoint delivers a new snapshot: forgets pins
    /// for workspaces that no longer exist (ids are reused after restarts), and
    /// sends at most one move when pinned workspaces are not all at the top.
    pub(crate) fn reconcile_pinned_workspaces(&mut self) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        if self.prune_pinned_workspaces() {
            self.pinned_repaired_orders.clear();
            self.persist_chrome_preferences(&mut outcome);
        }
        let roots = self.root_workspace_order();
        let endpoint_id = self.active_endpoint_id.clone();
        let boundary = roots
            .iter()
            .position(|id| !self.workspace_is_pinned(&endpoint_id, id));
        let misplaced = boundary.and_then(|boundary| {
            roots[boundary..]
                .iter()
                .find(|id| self.workspace_is_pinned(&endpoint_id, id))
                .map(|id| (id.clone(), roots[boundary].clone()))
        });
        let Some((workspace_id, before)) = misplaced else {
            return outcome;
        };
        // Only repair an order we have not already repaired, so a rejected or
        // pending move, or another client with different pins, cannot loop.
        if self.pinned_repaired_orders.contains(&roots) {
            return outcome;
        }
        let Some(method) = self.workspace_move_method(&workspace_id, Some(&before)) else {
            return outcome;
        };
        // Background repairs stay silent: no notices for offline or old servers.
        if !self.endpoint_is_online(&endpoint_id) || !self.supports_endpoint_method(&method) {
            return outcome;
        }
        self.pinned_repaired_orders.insert(roots);
        self.push_endpoint_method(method, &mut outcome);
        outcome
    }

    /// Ids of root workspaces (not linked worktrees) in server order.
    fn root_workspace_order(&self) -> Vec<String> {
        self.snapshot
            .as_deref()
            .map(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .filter(|workspace| {
                        !workspace
                            .worktree
                            .as_ref()
                            .is_some_and(|worktree| worktree.is_linked_worktree)
                    })
                    .map(|workspace| workspace.workspace_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Keeps a dragged workspace on its side of the pinned boundary: pinned
    /// workspaces cannot be dropped among unpinned ones, and unpinned
    /// workspaces cannot be dropped among pinned ones.
    pub(super) fn clamp_workspace_drop_target(
        &self,
        source_workspace_id: &str,
        before_workspace_id: Option<String>,
    ) -> Option<String> {
        let endpoint_id = &self.active_endpoint_id;
        let boundary = self.first_unpinned_root_workspace(source_workspace_id);
        let before_is_pinned = before_workspace_id
            .as_deref()
            .is_some_and(|before| self.workspace_is_pinned(endpoint_id, before));
        if self.workspace_is_pinned(endpoint_id, source_workspace_id) {
            if before_is_pinned || before_workspace_id == boundary {
                before_workspace_id
            } else {
                boundary
            }
        } else if before_is_pinned {
            boundary
        } else {
            before_workspace_id
        }
    }

    /// Drops ids of workspaces that no longer exist on the active endpoint so
    /// saved preferences do not grow and a reused id does not inherit a pin.
    /// An empty workspace list is treated as "not loaded yet" and ignored.
    /// Returns whether any pin was removed.
    fn prune_pinned_workspaces(&mut self) -> bool {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        if snapshot.workspaces.is_empty() {
            return false;
        }
        let live = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect::<HashSet<_>>();
        let endpoint_id = self.active_endpoint_id.clone();
        let pinned = self.pinned_workspaces_for_endpoint_mut(&endpoint_id);
        let before = pinned.len();
        pinned.retain(|id| live.contains(id));
        pinned.len() != before
    }
}
